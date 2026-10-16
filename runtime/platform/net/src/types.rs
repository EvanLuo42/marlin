use core::net::SocketAddr;
use core::time::Duration;

use crate::Result;

/// Most operations a [`NetQueue`](crate::NetQueue) may have in flight.
pub const MAX_QUEUE_CAPACITY: u32 = 16384;
/// Longest single send or receive. Longer sends are `InvalidInput`; longer receives are clamped.
pub const MAX_IO_LEN: usize = 0x7FFF_F000;

/// Which mechanism a [`NetQueue`](crate::NetQueue) uses.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine {
    /// Windows: overlapped Winsock (`AcceptEx`, `ConnectEx`, `WSASend`, `WSARecv`) on an IO
    /// completion port. Registered IO (RIO) is the planned faster engine.
    Iocp,
}

/// Which directions [`NetQueue::shutdown`](crate::NetQueue::shutdown) closes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shutdown {
    Read,
    /// Sends FIN once queued data is out: the peer's receives then return 0.
    Write,
    Both,
}

/// TCP keepalive probing of idle connections.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Keepalive {
    /// Idle time before the first probe. Whole seconds, at least 1.
    pub idle: Duration,
    /// Time between unanswered probes. Whole seconds, at least 1.
    pub interval: Duration,
    /// Unanswered probes before the connection is reset.
    pub retries: u32,
}

/// Per-connection options. The defaults suit request/response traffic: Nagle off, OS buffer
/// sizes, no keepalive, graceful close.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SocketOptions {
    /// `TCP_NODELAY`: send small writes at once instead of coalescing them (Nagle).
    pub nodelay: bool,
    /// `SO_SNDBUF` in bytes; `None` keeps the OS default (Windows autotunes it).
    pub send_buffer: Option<u32>,
    /// `SO_RCVBUF` in bytes; `None` keeps the OS default (autotuned).
    pub recv_buffer: Option<u32>,
    pub keepalive: Option<Keepalive>,
    /// `SO_LINGER`. `Some(ZERO)` makes closing reset the connection (RST) instead of sending FIN;
    /// `None` closes gracefully in the background.
    pub linger: Option<Duration>,
    /// Receives wait for data before they touch their buffer, then copy what is there. On Windows
    /// a zero-byte `WSARecv` followed by a non-blocking `recv`: a waiting receive pins no buffer
    /// pages, which suits many idle connections and small messages, at one more call per receive.
    /// Off, a receive hands its buffer to the OS up front, which suits bulk transfers.
    ///
    /// With several receives in flight on one socket, data goes to them in the order their
    /// wake-ups are processed rather than strictly in submission order.
    pub recv_when_readable: bool,
}

impl Default for SocketOptions {
    fn default() -> Self {
        Self {
            nodelay: true,
            send_buffer: None,
            recv_buffer: None,
            keepalive: None,
            linger: None,
            recv_when_readable: false,
        }
    }
}

/// Options of a listening socket.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ListenOptions {
    /// Connections the OS completes and holds before they are accepted. Clamped by the OS
    /// (Windows: 200..=65535).
    pub backlog: u32,
    /// Applied to every accepted connection.
    pub accepted: SocketOptions,
}

impl Default for ListenOptions {
    fn default() -> Self {
        Self {
            backlog: 1024,
            accepted: SocketOptions::default(),
        }
    }
}

/// A socket option as the backends set it: every value is an `int`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SockOpt {
    NoDelay,
    SendBuffer,
    RecvBuffer,
    KeepAlive,
    /// Seconds.
    KeepIdle,
    /// Seconds.
    KeepInterval,
    KeepCount,
    /// Seconds, or -1 for off.
    Linger,
    /// Not an OS option: a backend flag, see `SocketOptions::recv_when_readable`.
    RecvWhenReadable,
}

impl SocketOptions {
    /// Sets these options on `socket`. On a `fresh` socket, options still at the OS default are
    /// skipped (one syscall per accepted connection instead of five).
    pub(crate) fn apply<S>(
        &self,
        socket: &S,
        fresh: bool,
        set: impl Fn(&S, SockOpt, i32) -> Result<()>,
    ) -> Result<()> {
        if !fresh || self.nodelay {
            set(socket, SockOpt::NoDelay, i32::from(self.nodelay))?;
        }
        if let Some(n) = self.send_buffer {
            set(socket, SockOpt::SendBuffer, clamp(n))?;
        }
        if let Some(n) = self.recv_buffer {
            set(socket, SockOpt::RecvBuffer, clamp(n))?;
        }
        match self.keepalive {
            Some(k) => {
                set(socket, SockOpt::KeepIdle, seconds(k.idle))?;
                set(socket, SockOpt::KeepInterval, seconds(k.interval))?;
                set(socket, SockOpt::KeepCount, clamp(k.retries.max(1)))?;
                set(socket, SockOpt::KeepAlive, 1)?;
            }
            None if !fresh => set(socket, SockOpt::KeepAlive, 0)?,
            None => {}
        }
        match self.linger {
            Some(d) => set(
                socket,
                SockOpt::Linger,
                d.as_secs().min(u16::MAX.into()) as i32,
            )?,
            None if !fresh => set(socket, SockOpt::Linger, -1)?,
            None => {}
        }
        if !fresh || self.recv_when_readable {
            set(
                socket,
                SockOpt::RecvWhenReadable,
                i32::from(self.recv_when_readable),
            )?;
        }
        Ok(())
    }

    /// Reads the options back. Buffer sizes are what the OS reports (Linux doubles them).
    pub(crate) fn read<S>(socket: &S, get: impl Fn(&S, SockOpt) -> Result<i32>) -> Result<Self> {
        let keepalive = if get(socket, SockOpt::KeepAlive)? != 0 {
            Some(Keepalive {
                idle: Duration::from_secs(get(socket, SockOpt::KeepIdle)?.max(0) as u64),
                interval: Duration::from_secs(get(socket, SockOpt::KeepInterval)?.max(0) as u64),
                retries: get(socket, SockOpt::KeepCount)?.max(0) as u32,
            })
        } else {
            None
        };
        let linger = get(socket, SockOpt::Linger)?;
        Ok(Self {
            nodelay: get(socket, SockOpt::NoDelay)? != 0,
            send_buffer: Some(get(socket, SockOpt::SendBuffer)?.max(0) as u32),
            recv_buffer: Some(get(socket, SockOpt::RecvBuffer)?.max(0) as u32),
            keepalive,
            linger: (linger >= 0).then(|| Duration::from_secs(linger as u64)),
            recv_when_readable: get(socket, SockOpt::RecvWhenReadable)? != 0,
        })
    }
}

fn clamp(n: u32) -> i32 {
    n.min(i32::MAX as u32) as i32
}

fn seconds(d: Duration) -> i32 {
    let secs = d.as_secs() + u64::from(d.subsec_nanos() > 0);
    secs.clamp(1, i32::MAX as u64) as i32
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OpKind {
    Connect,
    Accept,
    Send,
    Recv,
}

/// One operation, as handed to the backend.
// The stub backend reads none of it.
#[cfg_attr(
    any(marlin_platform = "linux", marlin_platform = "macos"),
    allow(dead_code)
)]
pub(crate) struct Op<'a, S> {
    pub kind: OpKind,
    pub socket: &'a S,
    /// Send/receive buffer; null otherwise.
    pub buf: *mut u8,
    pub len: u32,
    /// Connect only: the peer.
    pub addr: Option<SocketAddr>,
    pub tag: u64,
}

/// A finished operation, as the backend reports it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RawCompletion {
    pub tag: u64,
    /// Bytes transferred, or the OS error.
    pub result: Result<usize>,
}

impl RawCompletion {
    pub const EMPTY: Self = Self {
        tag: 0,
        result: Ok(0),
    };
}
