//! A framed TCP server on marlin-net's completion queue: many connections, pipelined requests,
//! one thread. `marlin-store` and `marlin-cookd` both run on it.
//!
//! Each connection always has one receive in flight; arriving bytes are cut into frames
//! ([`marlin_asset::wire`]) and handed to the [`Service`]. Replies queue per connection and are
//! sent one at a time, in order. A malformed frame closes its connection.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use marlin_asset::wire::{self, Frame, FrameReader};
use marlin_net::{Completed, IoBuffer, ListenOptions, NetQueue, SocketId, SocketOptions};

/// A connection's id while it is open.
pub type ConnId = SocketId;

/// Receive buffer per connection.
const RECV_SIZE: usize = 256 << 10;
/// Accepts kept in flight, so a burst of connections is taken without a round trip each.
const ACCEPTS: usize = 4;
/// Queue capacity: a receive and a send per connection, plus the accepts.
const CAPACITY: u32 = 4096;

/// What a server does with requests.
pub trait Service {
    /// A frame arrived on `conn`. Reply (now or later) through `out`.
    fn request(&mut self, conn: ConnId, frame: Frame, out: &mut Outbox);

    /// Called every turn of the loop (at least every tick): for work finished elsewhere
    /// (worker threads, file watching).
    fn tick(&mut self, _out: &mut Outbox) {}

    fn opened(&mut self, _conn: ConnId) {}

    fn closed(&mut self, _conn: ConnId) {}
}

/// Frames to send and connections to close, collected while handling.
#[derive(Default)]
pub struct Outbox {
    sends: Vec<(ConnId, Vec<u8>)>,
    closes: Vec<ConnId>,
}

impl Outbox {
    pub fn send(&mut self, conn: ConnId, kind: u16, id: u32, payload: &[u8]) {
        self.sends.push((conn, wire::frame(kind, id, payload)));
    }

    pub fn close(&mut self, conn: ConnId) {
        self.closes.push(conn);
    }
}

struct Conn {
    reader: FrameReader,
    outgoing: VecDeque<Vec<u8>>,
    sending: bool,
    closing: bool,
}

pub struct Server {
    queue: NetQueue,
    listener: SocketId,
    addr: SocketAddr,
    conns: HashMap<ConnId, Conn>,
    /// Send buffers to reuse.
    spare: Vec<IoBuffer>,
}

#[derive(Debug, marlin_error::Error)]
#[error("network: {0}")]
pub struct ServerError(pub marlin_net::Error);

impl From<marlin_net::Error> for ServerError {
    fn from(e: marlin_net::Error) -> Self {
        Self(e)
    }
}

impl Server {
    /// Listens on `addr` (port 0: any free port; see [`local_addr`](Self::local_addr)).
    pub fn bind(addr: SocketAddr) -> Result<Self, ServerError> {
        let mut queue = NetQueue::new(CAPACITY)?;
        let listener = queue.listen(addr, &ListenOptions::default())?;
        let addr = queue.local_addr(listener)?;
        for _ in 0..ACCEPTS {
            queue.accept(listener)?;
        }
        Ok(Self {
            queue,
            listener,
            addr,
            conns: HashMap::new(),
            spare: Vec::new(),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn connections(&self) -> usize {
        self.conns.len()
    }

    /// Serves until `stop` is set, waking at least every `tick`.
    pub fn run(
        &mut self,
        service: &mut dyn Service,
        stop: &AtomicBool,
        tick: Duration,
    ) -> Result<(), ServerError> {
        let mut out = Outbox::default();
        while !stop.load(Ordering::Relaxed) {
            if let Some(done) = self.queue.wait_timeout(tick)? {
                self.complete(done, service, &mut out)?;
                // Take everything else that is ready before ticking.
                while let Some(done) = self.queue.poll()? {
                    self.complete(done, service, &mut out)?;
                }
            }
            service.tick(&mut out);
            self.flush(&mut out);
        }
        let open: Vec<ConnId> = self.conns.keys().copied().collect();
        for conn in open {
            let _ = self.queue.close(conn);
        }
        let _ = self.queue.close(self.listener);
        // Let the closes complete.
        while self.queue.in_flight() > 0 {
            if self
                .queue
                .wait_timeout(Duration::from_millis(100))?
                .is_none()
            {
                break;
            }
        }
        Ok(())
    }

    fn complete(
        &mut self,
        done: Completed,
        service: &mut dyn Service,
        out: &mut Outbox,
    ) -> Result<(), ServerError> {
        match done {
            Completed::Accepted {
                result, listener, ..
            } => {
                self.queue.accept(listener)?;
                if let Ok((conn, _peer)) = result {
                    let _ = self.queue.set_options(conn, &SocketOptions::default());
                    self.conns.insert(
                        conn,
                        Conn {
                            reader: FrameReader::new(),
                            outgoing: VecDeque::new(),
                            sending: false,
                            closing: false,
                        },
                    );
                    service.opened(conn);
                    self.receive(conn, IoBuffer::new(RECV_SIZE));
                }
            }
            Completed::Received {
                socket,
                buffer,
                start,
                result,
                ..
            } => {
                let Some(conn) = self.conns.get_mut(&socket) else {
                    return Ok(());
                };
                match result {
                    Ok(n) if n > 0 && !conn.closing => {
                        conn.reader.push(&buffer[start..start + n]);
                        loop {
                            match conn.reader.next_frame() {
                                Ok(Some(frame)) => service.request(socket, frame, out),
                                Ok(None) => break,
                                Err(_) => {
                                    out.close(socket);
                                    break;
                                }
                            }
                        }
                        self.receive(socket, buffer);
                    }
                    _ => self.close(socket),
                }
            }
            Completed::Sent {
                socket,
                buffer,
                result,
                ..
            } => {
                self.spare.push(buffer);
                match result {
                    Ok(_) => {
                        if let Some(conn) = self.conns.get_mut(&socket) {
                            conn.sending = false;
                        }
                        self.pump(socket);
                    }
                    Err(_) => self.close(socket),
                }
            }
            Completed::Closed { socket } => {
                if self.conns.remove(&socket).is_some() {
                    service.closed(socket);
                }
            }
            Completed::Connected { .. } => {}
        }
        Ok(())
    }

    fn receive(&mut self, conn: ConnId, buffer: IoBuffer) {
        if self.queue.recv(conn, buffer, ..).is_err() {
            self.close(conn);
        }
    }

    fn close(&mut self, conn: ConnId) {
        if let Some(c) = self.conns.get_mut(&conn)
            && !c.closing
        {
            c.closing = true;
            let _ = self.queue.close(conn);
        }
    }

    fn flush(&mut self, out: &mut Outbox) {
        for (conn, bytes) in out.sends.drain(..) {
            if let Some(c) = self.conns.get_mut(&conn)
                && !c.closing
            {
                c.outgoing.push_back(bytes);
            }
        }
        // Every connection with something to send: also those a full queue held back.
        let ready: Vec<ConnId> = self
            .conns
            .iter()
            .filter(|(_, c)| !c.sending && !c.outgoing.is_empty())
            .map(|(&k, _)| k)
            .collect();
        for conn in ready {
            self.pump(conn);
        }
        for conn in core::mem::take(&mut out.closes) {
            self.close(conn);
        }
    }

    /// Starts the next send of `conn` if none is in flight.
    fn pump(&mut self, conn: ConnId) {
        let Some(c) = self.conns.get_mut(&conn) else {
            return;
        };
        if c.sending || c.closing {
            return;
        }
        let Some(bytes) = c.outgoing.pop_front() else {
            return;
        };
        let mut buffer = match self
            .spare
            .iter()
            .position(|b| b.len() >= bytes.len() && b.len() <= bytes.len() * 2)
        {
            Some(i) => self.spare.swap_remove(i),
            None => IoBuffer::new(bytes.len().next_power_of_two().max(4096)),
        };
        buffer[..bytes.len()].copy_from_slice(&bytes);
        match self.queue.send(conn, buffer, ..bytes.len()) {
            Ok(_) => c.sending = true,
            Err(e) if e.error.kind() == marlin_net::ErrorKind::WouldBlock => {
                // Queue full: retry when something completes.
                c.outgoing.push_front(bytes);
                self.spare.push(e.buffer);
            }
            Err(_) => self.close(conn),
        }
        // Keep the spare pool small.
        self.spare.truncate(64);
    }
}
