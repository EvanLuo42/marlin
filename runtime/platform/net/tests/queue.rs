//! NetQueue over real loopback sockets.
#![cfg(marlin_platform = "windows")]

mod common;

use std::time::{Duration, Instant};

use common::{Mode, Server, any_port, buffer_from, connected_pair, next, pattern};
use marlin_net::{
    Completed, Engine, ErrorKind, IoBuffer, Keepalive, ListenOptions, NetQueue, Shutdown,
    SocketOptions,
};

#[test]
fn echo_in_one_queue() {
    let mut queue = NetQueue::new(16).unwrap();
    assert_eq!(queue.engine(), Engine::Iocp);
    let (client, server) = connected_pair(&mut queue, &SocketOptions::default());
    assert_eq!(
        queue.peer_addr(client).unwrap(),
        queue.local_addr(server).unwrap()
    );
    assert_eq!(
        queue.peer_addr(server).unwrap(),
        queue.local_addr(client).unwrap()
    );

    let message = b"hello, marlin";
    let recv = queue.recv(server, IoBuffer::new(64), 8..).unwrap();
    let send = queue.send(client, buffer_from(message), ..).unwrap();
    assert_eq!(queue.in_flight(), 2);
    let mut got = None;
    for _ in 0..2 {
        match next(&mut queue) {
            Completed::Sent { id, result, .. } => {
                assert_eq!(id, send);
                assert_eq!(result.unwrap(), message.len());
            }
            Completed::Received {
                id,
                socket,
                buffer,
                start,
                result,
            } => {
                assert_eq!((id, socket, start), (recv, server, 8));
                let n = result.unwrap();
                got = Some(buffer[start..start + n].to_vec());
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(got.unwrap(), message);
    assert_eq!(queue.in_flight(), 0);

    queue.close(client).unwrap();
    queue.close(server).unwrap();
    for _ in 0..2 {
        assert!(matches!(next(&mut queue), Completed::Closed { .. }));
    }
    assert!(queue.poll().unwrap().is_none());
}

#[test]
fn echo_server_thread() {
    let server = Server::start(Mode::Echo);
    let mut queue = NetQueue::new(8).unwrap();
    let (socket, _) = queue
        .connect(server.addr, &SocketOptions::default())
        .unwrap();
    assert!(matches!(
        next(&mut queue),
        Completed::Connected { result: Ok(()), .. }
    ));

    let data = pattern(200_000, 1);
    queue.send(socket, buffer_from(&data), ..).unwrap();
    let mut echoed = Vec::new();
    let mut buffer = Some(IoBuffer::new(4096));
    let mut sent = false;
    while echoed.len() < data.len() || !sent {
        if let Some(b) = buffer.take() {
            queue.recv(socket, b, ..).unwrap();
        }
        match next(&mut queue) {
            Completed::Sent { result, .. } => {
                assert_eq!(result.unwrap(), data.len());
                sent = true;
            }
            Completed::Received {
                buffer: b, result, ..
            } => {
                let n = result.unwrap();
                assert!(n > 0, "server closed early");
                echoed.extend_from_slice(&b[..n]);
                buffer = Some(b);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(echoed == data);
    drop(queue);
    let stats = server.stop();
    assert_eq!(stats.accepted, 1);
    assert_eq!(stats.received, data.len() as u64);
}

#[test]
fn ipv6_loopback() {
    let mut queue = NetQueue::new(8).unwrap();
    let listener = match queue.listen("[::1]:0".parse().unwrap(), &ListenOptions::default()) {
        Ok(listener) => listener,
        Err(e) if e.kind() == ErrorKind::AddrNotAvailable => {
            println!("no IPv6 loopback: {e}");
            return;
        }
        Err(e) => panic!("{e}"),
    };
    let addr = queue.local_addr(listener).unwrap();
    assert!(addr.is_ipv6());
    queue.accept(listener).unwrap();
    let (client, _) = queue.connect(addr, &SocketOptions::default()).unwrap();
    let mut accepted = None;
    for _ in 0..2 {
        match next(&mut queue) {
            Completed::Accepted { result, .. } => {
                let (socket, peer) = result.unwrap();
                assert_eq!(peer, queue.local_addr(client).unwrap());
                accepted = Some(socket);
            }
            Completed::Connected { result, .. } => result.unwrap(),
            other => panic!("unexpected {other:?}"),
        }
    }
    queue.send(client, buffer_from(b"v6"), ..).unwrap();
    queue.recv(accepted.unwrap(), IoBuffer::new(2), ..).unwrap();
    for _ in 0..2 {
        match next(&mut queue) {
            Completed::Received { buffer, result, .. } => {
                assert_eq!(&buffer[..result.unwrap()], b"v6")
            }
            Completed::Sent { result, .. } => assert_eq!(result.unwrap(), 2),
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn connection_refused() {
    let mut queue = NetQueue::new(4).unwrap();
    // A port that was just free: bind, read it back, close.
    let listener = queue.listen(any_port(), &ListenOptions::default()).unwrap();
    let addr = queue.local_addr(listener).unwrap();
    queue.close(listener).unwrap();
    assert!(matches!(next(&mut queue), Completed::Closed { .. }));

    let (socket, id) = queue.connect(addr, &SocketOptions::default()).unwrap();
    match next(&mut queue) {
        Completed::Connected {
            id: done,
            socket: s,
            result,
        } => {
            assert_eq!((done, s), (id, socket));
            assert_eq!(result.unwrap_err().kind(), ErrorKind::ConnectionRefused);
        }
        other => panic!("unexpected {other:?}"),
    }
    // The failed socket is still ours to close.
    queue.close(socket).unwrap();
    assert!(matches!(next(&mut queue), Completed::Closed { .. }));
}

#[test]
fn peer_reset_and_close() {
    let mut queue = NetQueue::new(8).unwrap();

    // Graceful: shutdown(Write) and close both read as end of stream.
    let (client, server) = connected_pair(&mut queue, &SocketOptions::default());
    queue.recv(client, IoBuffer::new(16), ..).unwrap();
    queue.shutdown(server, Shutdown::Write).unwrap();
    match next(&mut queue) {
        Completed::Received { result, .. } => assert_eq!(result.unwrap(), 0),
        other => panic!("unexpected {other:?}"),
    }
    // Sending after shutting down writing fails at once.
    queue.send(server, buffer_from(b"x"), ..).unwrap();
    match next(&mut queue) {
        Completed::Sent { result, .. } => {
            assert_eq!(result.unwrap_err().kind(), ErrorKind::BrokenPipe)
        }
        other => panic!("unexpected {other:?}"),
    }
    queue.close(server).unwrap();
    queue.close(client).unwrap();
    for _ in 0..2 {
        assert!(matches!(next(&mut queue), Completed::Closed { .. }));
    }

    // Abortive: linger 0 makes close send RST.
    let (client, server) = connected_pair(&mut queue, &SocketOptions::default());
    queue.recv(client, IoBuffer::new(16), ..).unwrap();
    queue
        .set_options(
            server,
            &SocketOptions {
                linger: Some(Duration::ZERO),
                ..SocketOptions::default()
            },
        )
        .unwrap();
    queue.close(server).unwrap();
    let mut reset = false;
    for _ in 0..2 {
        match next(&mut queue) {
            Completed::Closed { socket } => assert_eq!(socket, server),
            Completed::Received { result, .. } => {
                assert_eq!(result.unwrap_err().kind(), ErrorKind::ConnectionReset);
                reset = true;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(reset);
    // Sends on a reset connection fail too.
    queue.send(client, buffer_from(b"late"), ..).unwrap();
    match next(&mut queue) {
        Completed::Sent { result, .. } => {
            let kind = result.unwrap_err().kind();
            assert!(
                matches!(
                    kind,
                    ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
                ),
                "{kind:?}"
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn cancel_one_operation() {
    let mut queue = NetQueue::new(8).unwrap();
    let (client, server) = connected_pair(&mut queue, &SocketOptions::default());
    let id = queue.recv(server, IoBuffer::new(32), ..).unwrap();
    assert!(queue.poll().unwrap().is_none());
    queue.cancel(id);
    match next(&mut queue) {
        Completed::Received {
            id: done,
            buffer,
            result,
            ..
        } => {
            assert_eq!(done, id);
            assert_eq!(buffer.len(), 32);
            assert_eq!(result.unwrap_err().kind(), ErrorKind::Cancelled);
        }
        other => panic!("unexpected {other:?}"),
    }
    // Cancelling a finished id does nothing.
    queue.cancel(id);
    assert!(queue.poll().unwrap().is_none());

    // The connection still works.
    queue.recv(server, IoBuffer::new(32), ..).unwrap();
    queue.send(client, buffer_from(b"after"), ..).unwrap();
    for _ in 0..2 {
        match next(&mut queue) {
            Completed::Received { buffer, result, .. } => {
                assert_eq!(&buffer[..result.unwrap()], b"after")
            }
            Completed::Sent { result, .. } => assert_eq!(result.unwrap(), 5),
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn close_with_operations_in_flight() {
    let mut queue = NetQueue::new(16).unwrap();
    let (client, server) = connected_pair(&mut queue, &SocketOptions::default());
    let listener = queue.listen(any_port(), &ListenOptions::default()).unwrap();
    for _ in 0..4 {
        queue.accept(listener).unwrap();
    }
    for _ in 0..3 {
        queue.recv(server, IoBuffer::new(8), ..).unwrap();
    }
    assert_eq!(queue.in_flight(), 7);

    queue.close(listener).unwrap();
    queue.close(server).unwrap();
    // New operations on closing sockets are refused at once.
    let refused = queue.recv(server, IoBuffer::new(8), ..).unwrap_err();
    assert_eq!(refused.error.kind(), ErrorKind::InvalidInput);
    assert_eq!(
        queue.accept(listener).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        queue.close(server).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );

    let (mut accepts, mut recvs) = (0, 0);
    let mut closed = Vec::new();
    while closed.len() < 2 {
        match next(&mut queue) {
            Completed::Accepted {
                listener: l,
                result,
                ..
            } => {
                assert_eq!(l, listener);
                assert!(closed.is_empty() || closed == [server], "after Closed");
                assert_eq!(result.unwrap_err().kind(), ErrorKind::Cancelled);
                accepts += 1;
            }
            Completed::Received { socket, result, .. } => {
                assert_eq!(socket, server);
                assert!(!closed.contains(&server), "after Closed");
                assert_eq!(result.unwrap_err().kind(), ErrorKind::Cancelled);
                recvs += 1;
            }
            Completed::Closed { socket } => closed.push(socket),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!((accepts, recvs), (4, 3));
    assert_eq!(queue.in_flight(), 0);
    // Stale ids stay refused, even once the entries are reused.
    let (other, _) = connected_pair(&mut queue, &SocketOptions::default());
    assert_ne!(other, server);
    assert_eq!(
        queue.local_addr(server).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    // The peer sees the close.
    queue.recv(client, IoBuffer::new(8), ..).unwrap();
    match next(&mut queue) {
        Completed::Received { result, .. } => assert_eq!(result.unwrap(), 0),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn drop_with_operations_in_flight() {
    let server = Server::start(Mode::Echo);
    let start = Instant::now();
    for round in 0..20 {
        let mut queue = NetQueue::new(64).unwrap();
        let listener = queue.listen(any_port(), &ListenOptions::default()).unwrap();
        for _ in 0..8 {
            queue.accept(listener).unwrap();
        }
        // Connections mid-handshake, idle with receives pending, and mid-transfer.
        let mut sockets = Vec::new();
        for _ in 0..8 {
            sockets.push(
                queue
                    .connect(server.addr, &SocketOptions::default())
                    .unwrap()
                    .0,
            );
        }
        if round % 2 == 1 {
            let mut connected = 0;
            while connected < 8 {
                if let Completed::Connected { result, .. } = next(&mut queue) {
                    result.unwrap();
                    connected += 1;
                }
            }
            for &socket in &sockets {
                queue.recv(socket, IoBuffer::new(1024), ..).unwrap();
                queue
                    .send(socket, buffer_from(&pattern(1 << 20, 0)), ..)
                    .unwrap();
            }
        }
        drop(queue);
    }
    // Dropping cancels; it must not wait for the peer.
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn leaked_queue_is_safe() {
    let mut queue = NetQueue::new(4).unwrap();
    let (_, server) = connected_pair(&mut queue, &SocketOptions::default());
    queue.recv(server, IoBuffer::new(1024), ..).unwrap();
    // The buffer and the request leak with it, so the OS never writes into freed memory.
    std::mem::forget(queue);
}

#[test]
fn submit_errors_return_the_buffer() {
    let mut queue = NetQueue::new(2).unwrap();
    let (client, server) = connected_pair(&mut queue, &SocketOptions::default());

    let error = queue.send(client, IoBuffer::new(10), 4..4).unwrap_err();
    assert_eq!(error.error.kind(), ErrorKind::InvalidInput);
    assert_eq!(error.buffer.len(), 10);
    let error = queue.recv(client, IoBuffer::new(10), 5..11).unwrap_err();
    assert_eq!(error.error.kind(), ErrorKind::InvalidInput);

    queue.recv(server, IoBuffer::new(8), ..).unwrap();
    queue.recv(server, IoBuffer::new(8), ..).unwrap();
    let error = queue.send(client, IoBuffer::new(7), ..).unwrap_err();
    assert_eq!(error.error.kind(), ErrorKind::WouldBlock);
    assert_eq!(error.buffer.len(), 7);
    assert_eq!(
        queue
            .connect(any_port(), &SocketOptions::default())
            .unwrap_err()
            .kind(),
        ErrorKind::WouldBlock
    );
    assert!(matches!(
        NetQueue::new(0).map(|_| ()).unwrap_err().kind(),
        ErrorKind::InvalidInput
    ));
}

#[test]
fn options_round_trip() {
    let mut queue = NetQueue::new(4).unwrap();
    let options = SocketOptions {
        nodelay: false,
        send_buffer: Some(32 * 1024),
        recv_buffer: Some(48 * 1024),
        keepalive: Some(Keepalive {
            idle: Duration::from_secs(30),
            interval: Duration::from_secs(5),
            retries: 4,
        }),
        linger: Some(Duration::from_secs(2)),
        recv_when_readable: true,
    };
    // Applied at connect, and to accepted sockets through the listener.
    let (client, server) = connected_pair(&mut queue, &options);
    for socket in [client, server] {
        assert_eq!(queue.options(socket).unwrap(), options);
    }
    let defaults = SocketOptions::default();
    queue.set_options(client, &defaults).unwrap();
    let read = queue.options(client).unwrap();
    assert!(read.nodelay && !read.recv_when_readable);
    assert_eq!((read.keepalive, read.linger), (None, None));
}

/// Two-step receives: waiting before data, data already there, two receives racing for one
/// message, cancel, close, end of stream and reset.
#[test]
fn recv_when_readable() {
    let options = SocketOptions {
        recv_when_readable: true,
        ..SocketOptions::default()
    };
    let mut queue = NetQueue::new(16).unwrap();
    let (client, server) = connected_pair(&mut queue, &options);
    let received = |c: Completed| match c {
        Completed::Received {
            id,
            buffer,
            start,
            result,
            ..
        } => (id, buffer[start..start + result.unwrap()].to_vec()),
        other => panic!("unexpected {other:?}"),
    };

    // Waits before any data, then takes it.
    let waiting = queue.recv(server, IoBuffer::new(64), 4..).unwrap();
    assert!(queue.poll().unwrap().is_none());
    queue.send(client, buffer_from(b"first"), ..).unwrap();
    let mut got = Vec::new();
    for _ in 0..2 {
        match next(&mut queue) {
            Completed::Sent { .. } => {}
            c => got.push(received(c)),
        }
    }
    assert_eq!(got, [(waiting, b"first".to_vec())]);

    // Data already there.
    queue.send(client, buffer_from(b"second"), ..).unwrap();
    assert!(matches!(next(&mut queue), Completed::Sent { .. }));
    std::thread::sleep(Duration::from_millis(20));
    queue.recv(server, IoBuffer::new(64), ..).unwrap();
    assert_eq!(received(next(&mut queue)).1, b"second");

    // Two receives, one message: one gets it, the other keeps waiting for the next.
    let a = queue.recv(server, IoBuffer::new(64), ..).unwrap();
    let b = queue.recv(server, IoBuffer::new(64), ..).unwrap();
    queue.send(client, buffer_from(b"third"), ..).unwrap();
    let mut got = Vec::new();
    while got.is_empty() {
        match next(&mut queue) {
            Completed::Sent { .. } => {}
            c => got.push(received(c)),
        }
    }
    assert!(
        queue
            .wait_timeout(Duration::from_millis(50))
            .unwrap()
            .is_none()
    );
    queue.send(client, buffer_from(b"fourth"), ..).unwrap();
    while got.len() < 2 {
        match next(&mut queue) {
            Completed::Sent { .. } => {}
            c => got.push(received(c)),
        }
    }
    // Which receive gets which message is not specified; both are delivered, once.
    assert_ne!(got[0].0, got[1].0);
    assert!([a, b].contains(&got[0].0) && [a, b].contains(&got[1].0));
    assert_eq!(
        (&got[0].1[..], &got[1].1[..]),
        (&b"third"[..], &b"fourth"[..])
    );

    // Cancel while waiting.
    let id = queue.recv(server, IoBuffer::new(8), ..).unwrap();
    queue.cancel(id);
    match next(&mut queue) {
        Completed::Received { result, .. } => {
            assert_eq!(result.unwrap_err().kind(), ErrorKind::Cancelled)
        }
        other => panic!("unexpected {other:?}"),
    }

    // End of stream.
    queue.recv(server, IoBuffer::new(8), ..).unwrap();
    queue.shutdown(client, Shutdown::Write).unwrap();
    assert_eq!(received(next(&mut queue)).1, b"");

    // Reset, and close with a receive waiting.
    queue.recv(client, IoBuffer::new(8), ..).unwrap();
    queue
        .set_options(
            server,
            &SocketOptions {
                linger: Some(Duration::ZERO),
                ..options
            },
        )
        .unwrap();
    queue.close(server).unwrap();
    let mut reset = false;
    for _ in 0..2 {
        match next(&mut queue) {
            Completed::Closed { .. } => {}
            Completed::Received { result, .. } => {
                assert_eq!(result.unwrap_err().kind(), ErrorKind::ConnectionReset);
                reset = true;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(reset);
    let (c2, s2) = connected_pair(&mut queue, &options);
    queue.recv(s2, IoBuffer::new(8), ..).unwrap();
    queue.close(s2).unwrap();
    match next(&mut queue) {
        Completed::Received { result, .. } => {
            assert_eq!(result.unwrap_err().kind(), ErrorKind::Cancelled)
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(next(&mut queue), Completed::Closed { socket } if socket == s2));
    let _ = c2;
}

/// Accept on one queue, serve on another thread's queue: the server pattern.
#[test]
fn detach_and_attach() {
    let mut acceptor = NetQueue::new(4).unwrap();
    let listener = acceptor
        .listen(any_port(), &ListenOptions::default())
        .unwrap();
    let addr = acceptor.local_addr(listener).unwrap();
    acceptor.accept(listener).unwrap();

    let client = std::thread::spawn(move || {
        let mut stream = marlin_net::TcpStream::connect(addr).unwrap();
        stream.send_all(b"ping").unwrap();
        let mut reply = [0; 4];
        stream.recv_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"pong");
    });

    let socket = match next(&mut acceptor) {
        Completed::Accepted { result, .. } => result.unwrap().0,
        other => panic!("unexpected {other:?}"),
    };
    let detached = acceptor.detach(socket).unwrap();
    assert_eq!(
        acceptor.local_addr(socket).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    std::thread::spawn(move || {
        let mut worker = NetQueue::new(4).unwrap();
        let socket = worker.attach(detached);
        worker.recv(socket, IoBuffer::new(4), ..).unwrap();
        match next(&mut worker) {
            Completed::Received { buffer, result, .. } => {
                assert_eq!(&buffer[..result.unwrap()], b"ping")
            }
            other => panic!("unexpected {other:?}"),
        }
        worker.send(socket, buffer_from(b"pong"), ..).unwrap();
        assert!(matches!(
            next(&mut worker),
            Completed::Sent { result: Ok(4), .. }
        ));

        // Bound to this queue's completion port now: another queue refuses it.
        let moved = worker.detach(socket).unwrap();
        let mut other = NetQueue::new(4).unwrap();
        let socket = other.attach(moved);
        let refused = other.recv(socket, IoBuffer::new(4), ..).unwrap_err();
        assert_eq!(refused.error.kind(), ErrorKind::InvalidInput);
    })
    .join()
    .unwrap();
    client.join().unwrap();

    // Sockets with operations in flight stay put.
    acceptor.accept(listener).unwrap();
    assert_eq!(
        acceptor.detach(listener).unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
}
