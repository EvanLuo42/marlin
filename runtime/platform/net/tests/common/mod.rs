#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use marlin_math::Rng;
use marlin_net::{
    Completed, IoBuffer, ListenOptions, NetQueue, SocketAddr, SocketId, SocketOptions,
};

pub const LOCALHOST: &str = "127.0.0.1:0";

pub fn any_port() -> SocketAddr {
    LOCALHOST.parse().unwrap()
}

/// Deterministic, position-dependent bytes, so misplaced data is caught.
pub fn pattern(len: usize, seed: u32) -> Vec<u8> {
    let mut bytes = vec![0; len];
    Rng::new(seed.into()).fill_bytes(&mut bytes);
    bytes
}

pub fn buffer_from(data: &[u8]) -> IoBuffer {
    let mut buffer = IoBuffer::new(data.len());
    buffer.copy_from_slice(data);
    buffer
}

/// Waits for the next completion, failing the test after 10 s.
pub fn next(queue: &mut NetQueue) -> Completed {
    queue
        .wait_timeout(Duration::from_secs(10))
        .unwrap()
        .expect("no completion within 10 s")
}

/// Connects `queue` to a listener in the same queue: returns (client, server side).
pub fn connected_pair(queue: &mut NetQueue, options: &SocketOptions) -> (SocketId, SocketId) {
    let listener = queue
        .listen(
            any_port(),
            &ListenOptions {
                accepted: *options,
                ..ListenOptions::default()
            },
        )
        .unwrap();
    let addr = queue.local_addr(listener).unwrap();
    queue.accept(listener).unwrap();
    let (client, _) = queue.connect(addr, options).unwrap();
    let mut server = None;
    let mut connected = false;
    while server.is_none() || !connected {
        match next(queue) {
            Completed::Accepted { result, .. } => server = Some(result.unwrap().0),
            Completed::Connected { result, .. } => {
                result.unwrap();
                connected = true;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    queue.close(listener).unwrap();
    assert!(matches!(next(queue), Completed::Closed { socket } if socket == listener));
    (client, server.unwrap())
}

/// Frame layout of the request/response protocol: a little-endian `u32` body length, then the
/// body. The response to a request has the same length, every byte XOR 0x5A.
pub fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// Splits complete frames off the front of `pending`, calling `f` with each body.
pub fn drain_frames(pending: &mut Vec<u8>, mut f: impl FnMut(&[u8])) {
    let mut at = 0;
    while pending.len() - at >= 4 {
        let len = u32::from_le_bytes(pending[at..at + 4].try_into().unwrap()) as usize;
        if pending.len() - at - 4 < len {
            break;
        }
        f(&pending[at + 4..at + 4 + len]);
        at += 4 + len;
    }
    pending.drain(..at);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Sends back whatever arrives.
    Echo,
    /// Answers each request frame (see [`frame`]); every recv's answers go out as one send.
    Frames,
    /// Reads and drops everything; closes at the end of the stream.
    Discard,
}

#[derive(Default, Debug)]
pub struct ServerStats {
    pub accepted: usize,
    pub received: u64,
    /// Receives that returned data.
    pub recvs: u64,
    pub frames: u64,
}

/// A server on its own thread with its own queue. Stops (dropping the queue with operations in
/// flight) when dropped.
pub struct Server {
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<ServerStats>>,
}

const RECV_LEN: usize = 64 * 1024;

impl Server {
    pub fn start(mode: Mode) -> Self {
        Self::start_with(mode, SocketOptions::default())
    }

    pub fn start_with(mode: Mode, accepted: SocketOptions) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let thread = {
            let stop = stop.clone();
            std::thread::spawn(move || serve(mode, accepted, &stop, tx))
        };
        Self {
            addr: rx.recv().unwrap(),
            stop,
            thread: Some(thread),
        }
    }

    pub fn stop(mut self) -> ServerStats {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(
    mode: Mode,
    accepted: SocketOptions,
    stop: &AtomicBool,
    addr: mpsc::Sender<SocketAddr>,
) -> ServerStats {
    let mut queue = NetQueue::new(4096).unwrap();
    let listener = queue
        .listen(
            any_port(),
            &ListenOptions {
                backlog: 4096,
                accepted,
            },
        )
        .unwrap();
    addr.send(queue.local_addr(listener).unwrap()).unwrap();
    for _ in 0..64 {
        queue.accept(listener).unwrap();
    }
    let mut pool: Vec<IoBuffer> = Vec::new();
    let mut pending: HashMap<SocketId, Vec<u8>> = HashMap::new();
    let mut stats = ServerStats::default();

    while !stop.load(Ordering::Relaxed) {
        let Some(done) = queue.wait_timeout(Duration::from_millis(5)).unwrap() else {
            continue;
        };
        match done {
            Completed::Accepted {
                listener, result, ..
            } => {
                queue.accept(listener).unwrap();
                if let Ok((socket, _)) = result {
                    stats.accepted += 1;
                    pending.insert(socket, Vec::new());
                    let buffer = take(&mut pool, RECV_LEN);
                    queue.recv(socket, buffer, ..RECV_LEN).unwrap();
                }
            }
            Completed::Received {
                socket,
                buffer,
                result: Ok(n @ 1..),
                ..
            } => {
                stats.received += n as u64;
                stats.recvs += 1;
                match mode {
                    Mode::Echo => {
                        // Answer with this buffer, keep receiving into another.
                        let next = take(&mut pool, RECV_LEN);
                        queue.send(socket, buffer, ..n).unwrap();
                        queue.recv(socket, next, ..RECV_LEN).unwrap();
                    }
                    Mode::Frames => {
                        let input = pending.get_mut(&socket).unwrap();
                        input.extend_from_slice(&buffer[..n]);
                        let mut answer = Vec::new();
                        drain_frames(input, |body| {
                            stats.frames += 1;
                            answer.extend_from_slice(&(body.len() as u32).to_le_bytes());
                            answer.extend(body.iter().map(|b| b ^ 0x5A));
                        });
                        if !answer.is_empty() {
                            let mut out = take(&mut pool, answer.len());
                            out[..answer.len()].copy_from_slice(&answer);
                            queue.send(socket, out, ..answer.len()).unwrap();
                        }
                        queue.recv(socket, buffer, ..RECV_LEN).unwrap();
                    }
                    Mode::Discard => {
                        queue.recv(socket, buffer, ..).unwrap();
                    }
                }
            }
            Completed::Received { socket, buffer, .. } => {
                // End of stream or error.
                pool.push(buffer);
                let _ = queue.close(socket);
            }
            Completed::Sent {
                socket,
                buffer,
                result,
                ..
            } => {
                pool.push(buffer);
                if result.is_err() {
                    let _ = queue.close(socket);
                }
            }
            Completed::Closed { socket } => {
                pending.remove(&socket);
            }
            Completed::Connected { .. } => unreachable!(),
        }
    }
    stats
}

/// A pooled buffer of at least `len` bytes.
fn take(pool: &mut Vec<IoBuffer>, len: usize) -> IoBuffer {
    match pool.pop() {
        Some(buffer) if buffer.len() >= len => buffer,
        _ => IoBuffer::new(len.max(RECV_LEN)),
    }
}
