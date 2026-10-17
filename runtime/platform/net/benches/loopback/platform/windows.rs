//! Client and server each run one queue on one thread.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use marlin_bench::Bench;
use marlin_net::{Completed, IoBuffer, NetQueue, Shutdown, SocketId, SocketOptions};

use crate::support::{Mode, Server, drain_frames, frame, next};

const RUN: Duration = Duration::from_secs(3);

/// Pipelined small requests: `connections` x `depth` outstanding, `batch` requests per send.
/// Runs for [`RUN`]; returns the time taken and the requests answered in it.
fn requests(
    options: &SocketOptions,
    connections: usize,
    depth: usize,
    batch: usize,
    body_len: usize,
) -> (Duration, u64) {
    let server = Server::start_with(Mode::Frames, *options);
    // A send's completion may be reported after its answers, so the window alone does not bound
    // the sends in flight; this does.
    let max_sends = 2 * depth.div_ceil(batch);
    let mut queue = NetQueue::new((connections * (max_sends + 1)) as u32).unwrap();
    let mut sockets = Vec::new();
    for _ in 0..connections {
        sockets.push(queue.connect(server.addr, options).unwrap().0);
    }
    for _ in 0..connections {
        assert!(matches!(
            next(&mut queue),
            Completed::Connected { result: Ok(()), .. }
        ));
    }

    let request = frame(&vec![0xA5; body_len]);
    let batch_bytes: Vec<u8> = request
        .iter()
        .copied()
        .cycle()
        .take(request.len() * batch)
        .collect();
    #[derive(Default)]
    struct Conn {
        /// Requests sent and not answered.
        outstanding: usize,
        /// Sends whose completions have not been reported.
        sending: usize,
        pending: Vec<u8>,
    }
    let mut pool: Vec<IoBuffer> = Vec::new();
    // Tops the window up in sends of `batch` requests.
    let top_up = |queue: &mut NetQueue, pool: &mut Vec<IoBuffer>, socket, conn: &mut Conn| {
        while depth - conn.outstanding >= batch.min(depth) && conn.sending < max_sends {
            let count = (depth - conn.outstanding).min(batch);
            // Every pooled buffer holds `batch` copies of the request.
            let buffer = pool.pop().unwrap_or_else(|| {
                let mut b = IoBuffer::new(batch_bytes.len());
                b.copy_from_slice(&batch_bytes);
                b
            });
            queue.send(socket, buffer, ..request.len() * count).unwrap();
            conn.outstanding += count;
            conn.sending += 1;
        }
    };
    let mut conns: HashMap<SocketId, Conn> = HashMap::new();
    for &socket in &sockets {
        let mut conn = Conn::default();
        top_up(&mut queue, &mut pool, socket, &mut conn);
        conns.insert(socket, conn);
        queue.recv(socket, IoBuffer::new(256 * 1024), ..).unwrap();
    }

    let mut answered = 0u64;
    let start = Instant::now();
    let mut measured = None;
    while measured.is_none() {
        match next(&mut queue) {
            Completed::Sent {
                socket,
                buffer,
                result,
                ..
            } => {
                result.unwrap();
                pool.push(buffer);
                let conn = conns.get_mut(&socket).unwrap();
                conn.sending -= 1;
                top_up(&mut queue, &mut pool, socket, conn);
            }
            Completed::Received {
                socket,
                buffer,
                result,
                ..
            } => {
                let n = result.unwrap();
                assert!(n > 0);
                let conn = conns.get_mut(&socket).unwrap();
                conn.pending.extend_from_slice(&buffer[..n]);
                let mut got = 0;
                drain_frames(&mut conn.pending, |_| got += 1);
                answered += got as u64;
                conn.outstanding -= got;
                top_up(&mut queue, &mut pool, socket, conn);
                queue.recv(socket, buffer, ..).unwrap();
                if start.elapsed() >= RUN {
                    measured = Some((start.elapsed(), answered));
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    drop(queue);
    let stats = server.stop();
    assert!(stats.frames >= answered);
    measured.unwrap()
}

/// One-way bulk transfer of `total` bytes over `connections`, `in_flight` sends of `chunk`
/// bytes each per connection, until the server has read everything. Returns the time taken.
fn bulk(
    options: &SocketOptions,
    connections: usize,
    total: usize,
    chunk: usize,
    in_flight: usize,
) -> Duration {
    let server = Server::start_with(Mode::Discard, *options);
    let mut queue = NetQueue::new((connections * (in_flight + 1)) as u32).unwrap();
    let mut sockets = Vec::new();
    for _ in 0..connections {
        sockets.push(queue.connect(server.addr, options).unwrap().0);
    }
    for _ in 0..connections {
        assert!(matches!(
            next(&mut queue),
            Completed::Connected { result: Ok(()), .. }
        ));
    }
    let per_conn = total / connections;
    // Bytes not yet sent and sends in flight, per connection.
    let mut state: HashMap<SocketId, (usize, usize)> =
        sockets.iter().map(|&s| (s, (per_conn, 0))).collect();
    let start = Instant::now();
    for &socket in &sockets {
        let (left, sending) = state.get_mut(&socket).unwrap();
        for _ in 0..in_flight {
            let n = chunk.min(*left);
            *left -= n;
            *sending += 1;
            queue.send(socket, IoBuffer::new(chunk), ..n).unwrap();
        }
        queue.recv(socket, IoBuffer::new(16), ..).unwrap();
    }
    let mut open = connections;
    while open > 0 {
        match next(&mut queue) {
            Completed::Sent {
                socket,
                buffer,
                result,
                ..
            } => {
                result.unwrap();
                let (left, sending) = state.get_mut(&socket).unwrap();
                if *left > 0 {
                    let n = chunk.min(*left);
                    *left -= n;
                    queue.send(socket, buffer, ..n).unwrap();
                } else {
                    *sending -= 1;
                    if *sending == 0 {
                        // Everything is out: end the stream.
                        queue.shutdown(socket, Shutdown::Write).unwrap();
                    }
                }
            }
            Completed::Received { result, .. } => {
                // The server closes after reading everything.
                assert_eq!(result.unwrap(), 0);
                open -= 1;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    let elapsed = start.elapsed();
    drop(queue);
    let stats = server.stop();
    assert_eq!(stats.received, (per_conn * connections) as u64);
    elapsed
}

/// Both receive modes; see `SocketOptions::recv_when_readable`.
fn modes() -> [(&'static str, SocketOptions); 2] {
    [("buffer_first", false), ("when_readable", true)].map(|(name, recv_when_readable)| {
        (
            name,
            SocketOptions {
                recv_when_readable,
                ..SocketOptions::default()
            },
        )
    })
}

/// Requests per second, 16 B bodies, pipelined.
pub fn requests_per_second() {
    for (connections, depth, batch) in [(1, 64, 1), (32, 64, 1), (1, 64, 64), (32, 64, 16)] {
        for (mode, options) in modes() {
            Bench::new(format!(
                "requests/{connections}conn_{depth}deep_{batch}per_send/{mode}"
            ))
            .throughput_items(1, "req")
            .warmup(Duration::ZERO)
            .samples(3)
            .run_custom(|| requests(&options, connections, depth, batch, 16));
        }
    }
}

/// Bulk transfer of 8 GiB in 1 MiB sends, 4 in flight per connection.
pub fn bulk_transfer() {
    const TOTAL: usize = 8 << 30;
    for connections in [1, 4] {
        for (mode, options) in modes() {
            Bench::new(format!("bulk/{connections}conn/{mode}"))
                .throughput_bytes(TOTAL as u64)
                .warmup(Duration::ZERO)
                .samples(3)
                .run_custom(|| (bulk(&options, connections, TOTAL, 1 << 20, 4), 1));
        }
    }
}
