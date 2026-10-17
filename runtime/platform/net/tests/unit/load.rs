//! Many connections, large transfers and pipelining over real loopback sockets.

use std::collections::{HashMap, VecDeque};

use marlin_net::{Completed, ErrorKind, IoBuffer, NetQueue, RequestId, SocketId, SocketOptions};

use crate::support::{
    Mode, Server, buffer_from, connected_pair, drain_frames, frame, next, pattern,
};

/// Receives that wait for data before taking their buffer, on both ends.
const READABLE: SocketOptions = SocketOptions {
    nodelay: true,
    send_buffer: None,
    recv_buffer: None,
    keepalive: None,
    linger: None,
    recv_when_readable: true,
};

/// 512 connections open at once, each echoing its own message concurrently.
#[test]
fn hundreds_of_connections() {
    hundreds_of_connections_with(SocketOptions::default());
}

#[test]
fn hundreds_of_connections_recv_when_readable() {
    hundreds_of_connections_with(READABLE);
}

fn hundreds_of_connections_with(options: SocketOptions) {
    const CONNECTIONS: usize = 512;
    const MESSAGE: usize = 3000;
    let server = Server::start_with(Mode::Echo, options);
    let mut queue = NetQueue::new(2 * CONNECTIONS as u32).unwrap();

    // Connect in waves so the listen backlog never overflows (Windows refuses, not queues, SYNs
    // past it); every connection stays open.
    let mut sockets = Vec::new();
    let mut connecting = 0;
    while sockets.len() < CONNECTIONS || connecting > 0 {
        while sockets.len() < CONNECTIONS && connecting < 128 {
            let (socket, _) = queue.connect(server.addr, &options).unwrap();
            sockets.push(socket);
            connecting += 1;
        }
        match next(&mut queue) {
            Completed::Connected { result, .. } => result.unwrap(),
            other => panic!("unexpected {other:?}"),
        }
        connecting -= 1;
    }

    let messages: HashMap<SocketId, Vec<u8>> = sockets
        .iter()
        .enumerate()
        .map(|(i, &s)| (s, pattern(MESSAGE, i as u32)))
        .collect();
    for (&socket, message) in &messages {
        queue.send(socket, buffer_from(message), ..).unwrap();
        queue.recv(socket, IoBuffer::new(MESSAGE), ..).unwrap();
    }
    let mut echoed: HashMap<SocketId, Vec<u8>> = HashMap::new();
    let mut complete = 0;
    let mut sends = 0;
    while complete < CONNECTIONS || sends < CONNECTIONS {
        match next(&mut queue) {
            Completed::Sent { result, .. } => {
                assert_eq!(result.unwrap(), MESSAGE);
                sends += 1;
            }
            Completed::Received {
                socket,
                buffer,
                result,
                ..
            } => {
                let n = result.unwrap();
                assert!(n > 0);
                let got = echoed.entry(socket).or_default();
                got.extend_from_slice(&buffer[..n]);
                if got.len() < MESSAGE {
                    queue.recv(socket, buffer, ..MESSAGE - got.len()).unwrap();
                } else {
                    assert!(*got == messages[&socket], "connection mixed up data");
                    complete += 1;
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    for &socket in &sockets {
        queue.close(socket).unwrap();
    }
    for _ in 0..CONNECTIONS {
        assert!(matches!(next(&mut queue), Completed::Closed { .. }));
    }
    drop(queue);
    let stats = server.stop();
    assert_eq!(stats.accepted, CONNECTIONS);
    assert_eq!(stats.received, (CONNECTIONS * MESSAGE) as u64);
}

/// 64 MiB each way at once through small socket buffers: sends larger than the socket buffer,
/// receives that come back short, several of each in flight.
#[test]
fn large_transfer_both_ways() {
    large_transfer(false, 2);
}

/// One receive per socket: with `recv_when_readable`, concurrent receives are not ordered.
#[test]
fn large_transfer_recv_when_readable() {
    large_transfer(true, 1);
}

fn large_transfer(recv_when_readable: bool, recvs_in_flight: usize) {
    const TOTAL: usize = 64 << 20;
    const SEND: usize = 4 << 20;
    const RECV: usize = 100_003;
    let small = SocketOptions {
        send_buffer: Some(16 * 1024),
        recv_buffer: Some(16 * 1024),
        recv_when_readable,
        ..SocketOptions::default()
    };
    let mut queue = NetQueue::new(32).unwrap();
    let (a, b) = connected_pair(&mut queue, &small);
    let data = [pattern(TOTAL, 7), pattern(TOTAL, 8)];

    struct Side {
        socket: SocketId,
        sent: usize,
        received: Vec<u8>,
        recvs: VecDeque<RequestId>,
        short: usize,
    }
    let mut sides = [a, b].map(|socket| Side {
        socket,
        sent: 0,
        received: Vec::with_capacity(TOTAL),
        recvs: VecDeque::new(),
        short: 0,
    });
    let by_socket = HashMap::from([(a, 0), (b, 1)]);
    let mut done = 0;
    // Two sends and `recvs_in_flight` receives in flight per side. Receives are reordered by id,
    // since completions may overtake each other.
    let mut early: HashMap<RequestId, (IoBuffer, usize)> = HashMap::new();
    for side in &mut sides {
        for _ in 0..recvs_in_flight {
            let id = queue.recv(side.socket, IoBuffer::new(RECV), ..).unwrap();
            side.recvs.push_back(id);
        }
    }
    let send_next = |queue: &mut NetQueue, side: &mut Side, me: usize| {
        if side.sent < TOTAL {
            let chunk = &data[me][side.sent..TOTAL.min(side.sent + SEND)];
            queue.send(side.socket, buffer_from(chunk), ..).unwrap();
            side.sent += chunk.len();
        }
    };
    for (me, side) in sides.iter_mut().enumerate() {
        send_next(&mut queue, side, me);
        send_next(&mut queue, side, me);
    }
    while done < 2 {
        match next(&mut queue) {
            Completed::Sent { socket, result, .. } => {
                assert!(result.unwrap() > 0);
                let me = by_socket[&socket];
                send_next(&mut queue, &mut sides[me], me);
            }
            Completed::Received {
                id,
                socket,
                buffer,
                result,
                ..
            } => {
                let me = by_socket[&socket];
                let n = result.unwrap();
                assert!(n > 0, "unexpected end of stream");
                early.insert(id, (buffer, n));
                let side = &mut sides[me];
                while let Some(&first) = side.recvs.front()
                    && let Some((buffer, n)) = early.remove(&first)
                {
                    side.recvs.pop_front();
                    side.short += usize::from(n < RECV);
                    side.received.extend_from_slice(&buffer[..n]);
                    let wanted = TOTAL - side.received.len();
                    let outstanding = side.recvs.len() * RECV;
                    if wanted > outstanding {
                        let id = queue.recv(side.socket, buffer, ..RECV).unwrap();
                        side.recvs.push_back(id);
                    } else if side.recvs.is_empty() {
                        done += 1;
                    }
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    for (me, side) in sides.iter().enumerate() {
        let from = 1 - me;
        assert_eq!(side.received.len(), TOTAL);
        assert!(
            side.received == data[from],
            "side {me} received corrupt data"
        );
        assert!(side.short > 0, "no short receive");
        println!("side {me}: {} short receives", side.short);
    }
}

/// 20 000 requests sent as individual operations, up to 256 unanswered, against a server that
/// parses frames and answers each read's requests with one send.
#[test]
fn pipelined_requests() {
    pipelined_requests_with(SocketOptions::default());
}

#[test]
fn pipelined_requests_recv_when_readable() {
    pipelined_requests_with(READABLE);
}

fn pipelined_requests_with(options: SocketOptions) {
    const REQUESTS: usize = 20_000;
    const WINDOW: usize = 256;
    let server = Server::start_with(Mode::Frames, options);
    let mut queue = NetQueue::new(WINDOW as u32 + 1).unwrap();
    let (socket, _) = queue.connect(server.addr, &options).unwrap();
    assert!(matches!(
        next(&mut queue),
        Completed::Connected { result: Ok(()), .. }
    ));

    let body = |i: usize| pattern(1 + i % 700, i as u32);
    let mut sent = 0;
    let mut answered = 0;
    let mut pending = Vec::new();
    queue.recv(socket, IoBuffer::new(64 * 1024), ..).unwrap();
    while answered < REQUESTS {
        while sent < REQUESTS && sent - answered < WINDOW && queue.in_flight() < queue.capacity() {
            queue
                .send(socket, buffer_from(&frame(&body(sent))), ..)
                .unwrap();
            sent += 1;
        }
        match next(&mut queue) {
            Completed::Sent { result, .. } => {
                result.unwrap();
            }
            Completed::Received { buffer, result, .. } => {
                let n = result.unwrap();
                assert!(n > 0, "server closed");
                pending.extend_from_slice(&buffer[..n]);
                drain_frames(&mut pending, |answer| {
                    let expected: Vec<u8> = body(answered).iter().map(|b| b ^ 0x5A).collect();
                    assert!(
                        answer == expected,
                        "answer {answered} out of order or corrupt"
                    );
                    answered += 1;
                });
                queue.recv(socket, buffer, ..).unwrap();
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(pending.is_empty());
    drop(queue);
    let stats = server.stop();
    assert_eq!(stats.frames, REQUESTS as u64);
}

/// The same request stream from a blocking client, which batches nothing.
#[test]
fn blocking_request_response() {
    let server = Server::start(Mode::Frames);
    let mut stream = marlin_net::TcpStream::connect(server.addr).unwrap();
    for i in 0..500 {
        let request = pattern(1 + i * 37 % 5000, i as u32);
        stream.send_all(&frame(&request)).unwrap();
        let mut header = [0; 4];
        stream.recv_exact(&mut header).unwrap();
        let mut answer = vec![0; u32::from_le_bytes(header) as usize];
        stream.recv_exact(&mut answer).unwrap();
        assert!(answer.iter().zip(&request).all(|(a, r)| a ^ 0x5A == *r));
        assert_eq!(answer.len(), request.len());
    }
    stream.shutdown(marlin_net::Shutdown::Write).unwrap();
    let mut rest = [0; 1];
    assert_eq!(stream.recv(&mut rest).unwrap(), 0);
    assert_eq!(
        stream.recv_exact(&mut rest).unwrap_err().kind(),
        ErrorKind::UnexpectedEof
    );
}
