//! Blocking sockets on top of [`NetQueue`]: one small queue per stream or listener.

use core::net::SocketAddr;
use core::ops::RangeBounds;
use core::time::Duration;

use marlin_io::IoBuffer;

use crate::{
    Completed, Error, ErrorKind, ListenOptions, NetQueue, RequestId, Result, Shutdown, Socket,
    SocketId, SocketOptions, SubmitError, resolve,
};

/// Bounce buffer for the slice-based calls.
const SCRATCH_LEN: usize = 256 * 1024;

/// A blocking TCP connection: for tools and simple clients. Pipelining and many connections
/// belong on a shared [`NetQueue`].
pub struct TcpStream {
    queue: NetQueue,
    socket: SocketId,
    scratch: Option<IoBuffer>,
    timeout: Option<Duration>,
}

impl TcpStream {
    /// Connects with the default options and no timeout.
    pub fn connect(addr: SocketAddr) -> Result<Self> {
        Self::connect_with(addr, &SocketOptions::default(), None)
    }

    /// Connects, giving up with `TimedOut` after `timeout`. The timeout also applies to every
    /// later send and receive; see [`set_timeout`](Self::set_timeout).
    pub fn connect_with(
        addr: SocketAddr,
        options: &SocketOptions,
        timeout: Option<Duration>,
    ) -> Result<Self> {
        let mut queue = NetQueue::new(1)?;
        let (socket, id) = queue.connect(addr, options)?;
        let mut stream = Self {
            queue,
            socket,
            scratch: None,
            timeout,
        };
        match stream.finish(id)? {
            Completed::Connected { result, .. } => result?,
            _ => unreachable!(),
        }
        Ok(stream)
    }

    /// Resolves `host` and connects to its addresses in turn, returning the first connection or
    /// the last error.
    pub fn connect_host(
        host: &str,
        port: u16,
        options: &SocketOptions,
        timeout: Option<Duration>,
    ) -> Result<Self> {
        let mut last = Error::from_kind(ErrorKind::NotFound);
        for addr in resolve(host, port)? {
            match Self::connect_with(addr, options, timeout) {
                Ok(stream) => return Ok(stream),
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    /// Wraps a connected socket, such as one from [`NetQueue::detach`]. There is no way back:
    /// on Windows the stream's first operation binds the socket to the stream's queue for life.
    pub fn from_socket(socket: Socket) -> Result<Self> {
        let mut queue = NetQueue::new(1)?;
        let socket = queue.attach(socket);
        Ok(Self {
            queue,
            socket,
            scratch: None,
            timeout: None,
        })
    }

    /// Bounds every later send and receive call: past it they cancel and fail with `TimedOut`.
    /// `None` waits forever.
    pub fn set_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout;
    }

    /// Sends all of `data`.
    pub fn send_all(&mut self, data: &[u8]) -> Result<()> {
        let mut scratch = self.take_scratch();
        for chunk in data.chunks(scratch.len()) {
            scratch[..chunk.len()].copy_from_slice(chunk);
            match self.send_all_buffer(scratch, ..chunk.len()) {
                Ok(buffer) => scratch = buffer,
                Err(error) => {
                    self.scratch = Some(error.buffer);
                    return Err(error.error);
                }
            }
        }
        self.scratch = Some(scratch);
        Ok(())
    }

    /// Sends all of `buffer[range]` without copying it.
    pub fn send_all_buffer(
        &mut self,
        buffer: IoBuffer,
        range: impl RangeBounds<usize>,
    ) -> Result<IoBuffer, SubmitError> {
        let id = self.queue.send(self.socket, buffer, range)?;
        match self.finish(id) {
            Ok(Completed::Sent { buffer, result, .. }) => match result {
                Ok(_) => Ok(buffer),
                Err(error) => Err(SubmitError { error, buffer }),
            },
            Ok(_) => unreachable!(),
            Err(error) => Err(lost(error)),
        }
    }

    /// Receives up to `buf.len()` bytes, whatever arrives first. `Ok(0)` at the end of the stream
    /// (or for an empty `buf`).
    pub fn recv(&mut self, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let scratch = self.take_scratch();
        let len = buf.len().min(scratch.len());
        let (scratch, result) = self.recv_buffer(scratch, ..len);
        if let Ok(n) = result {
            buf[..n].copy_from_slice(&scratch[..n]);
        }
        self.scratch = Some(scratch);
        result
    }

    /// Fills `buf`, or fails with `UnexpectedEof` if the stream ends first.
    pub fn recv_exact(&mut self, mut buf: &mut [u8]) -> Result<()> {
        while !buf.is_empty() {
            match self.recv(buf)? {
                0 => return Err(Error::from_kind(ErrorKind::UnexpectedEof)),
                n => buf = &mut buf[n..],
            }
        }
        Ok(())
    }

    /// Receives into `buffer[range]` without copying: whatever arrives first, `Ok(0)` at the end
    /// of the stream. Returns the buffer in every case.
    pub fn recv_buffer(
        &mut self,
        buffer: IoBuffer,
        range: impl RangeBounds<usize>,
    ) -> (IoBuffer, Result<usize>) {
        let id = match self.queue.recv(self.socket, buffer, range) {
            Ok(id) => id,
            Err(error) => return (error.buffer, Err(error.error)),
        };
        match self.finish(id) {
            Ok(Completed::Received { buffer, result, .. }) => (buffer, result),
            Ok(_) => unreachable!(),
            Err(error) => (IoBuffer::new(0), Err(error)),
        }
    }

    /// Fills `buffer[range]` without copying, or fails with `UnexpectedEof` if the stream ends
    /// first.
    pub fn recv_exact_buffer(
        &mut self,
        mut buffer: IoBuffer,
        range: impl RangeBounds<usize>,
    ) -> Result<IoBuffer, SubmitError> {
        let Some((mut start, end)) = crate::queue::bounds(&range, buffer.len()) else {
            return Err(SubmitError {
                error: Error::from_kind(ErrorKind::InvalidInput),
                buffer,
            });
        };
        while start < end {
            let result;
            (buffer, result) = self.recv_buffer(buffer, start..end);
            match result {
                Ok(0) => {
                    return Err(SubmitError {
                        error: Error::from_kind(ErrorKind::UnexpectedEof),
                        buffer,
                    });
                }
                Ok(n) => start += n,
                Err(error) => return Err(SubmitError { error, buffer }),
            }
        }
        Ok(buffer)
    }

    pub fn shutdown(&mut self, how: Shutdown) -> Result<()> {
        self.queue.shutdown(self.socket, how)
    }

    pub fn set_options(&mut self, options: &SocketOptions) -> Result<()> {
        self.queue.set_options(self.socket, options)
    }

    pub fn options(&self) -> Result<SocketOptions> {
        self.queue.options(self.socket)
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.queue.local_addr(self.socket)
    }

    pub fn peer_addr(&self) -> Result<SocketAddr> {
        self.queue.peer_addr(self.socket)
    }

    fn take_scratch(&mut self) -> IoBuffer {
        self.scratch
            .take()
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| IoBuffer::new(SCRATCH_LEN))
    }

    /// Waits for `id`, the only operation in flight, cancelling it at the timeout.
    fn finish(&mut self, id: RequestId) -> Result<Completed> {
        let mut completed = match self.timeout {
            Some(timeout) => match self.queue.wait_timeout(timeout)? {
                Some(completed) => return Ok(completed),
                None => {
                    self.queue.cancel(id);
                    self.queue.wait()?.unwrap()
                }
            },
            None => return Ok(self.queue.wait()?.unwrap()),
        };
        match &mut completed {
            Completed::Sent { result, .. } | Completed::Received { result, .. } => {
                timed_out(result)
            }
            Completed::Connected { result, .. } => timed_out(result),
            _ => {}
        }
        Ok(completed)
    }
}

/// Cancelled by us: report the timeout. An operation that won the race stands.
fn timed_out<T>(result: &mut Result<T>) {
    if matches!(result, Err(e) if e.kind() == ErrorKind::Cancelled) {
        *result = Err(Error::from_kind(ErrorKind::TimedOut));
    }
}

/// A blocking TCP listener.
pub struct TcpListener {
    queue: NetQueue,
    socket: SocketId,
}

impl TcpListener {
    /// Listens on `addr` with the default options. Port 0 picks a free port.
    pub fn bind(addr: SocketAddr) -> Result<Self> {
        Self::bind_with(addr, &ListenOptions::default())
    }

    pub fn bind_with(addr: SocketAddr, options: &ListenOptions) -> Result<Self> {
        let mut queue = NetQueue::new(1)?;
        let socket = queue.listen(addr, options)?;
        Ok(Self { queue, socket })
    }

    /// Waits for a connection.
    pub fn accept(&mut self) -> Result<(TcpStream, SocketAddr)> {
        self.queue.accept(self.socket)?;
        match self.queue.wait()?.unwrap() {
            Completed::Accepted { result, .. } => {
                let (socket, peer) = result?;
                let socket = self.queue.detach(socket)?;
                Ok((TcpStream::from_socket(socket)?, peer))
            }
            _ => unreachable!(),
        }
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.queue.local_addr(self.socket)
    }
}

/// The queue failed while the buffer was in flight; it stays with the queue until it drops.
fn lost(error: Error) -> SubmitError {
    SubmitError {
        error,
        buffer: IoBuffer::new(0),
    }
}
