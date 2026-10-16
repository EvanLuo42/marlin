pub type Result<T, E = Error> = core::result::Result<T, E>;

/// A network failure: a portable [`ErrorKind`] plus the OS error code (`WSAGetLastError`,
/// `errno`, console SDK result). `Copy` and allocation-free, like `marlin_pal::Error`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_error::Error)]
#[error(code = "os error {}")]
pub struct Error {
    kind: ErrorKind,
    raw: i32,
}

impl Error {
    #[inline]
    pub const fn new(kind: ErrorKind, raw: i32) -> Self {
        Self { kind, raw }
    }

    /// An error detected by this crate rather than reported by the OS.
    #[inline]
    pub const fn from_kind(kind: ErrorKind) -> Self {
        Self { kind, raw: 0 }
    }

    #[inline]
    pub const fn kind(self) -> ErrorKind {
        self.kind
    }

    /// The platform error code, or 0.
    #[inline]
    pub const fn raw_os_error(self) -> i32 {
        self.raw
    }
}

impl From<marlin_pal::Error> for Error {
    fn from(error: marlin_pal::Error) -> Self {
        use marlin_pal::ErrorKind as Pal;
        let kind = match error.kind() {
            Pal::OutOfMemory => ErrorKind::OutOfMemory,
            Pal::InvalidInput => ErrorKind::InvalidInput,
            Pal::PermissionDenied => ErrorKind::PermissionDenied,
            Pal::NotFound => ErrorKind::NotFound,
            Pal::WouldBlock => ErrorKind::WouldBlock,
            Pal::TimedOut => ErrorKind::TimedOut,
            Pal::Interrupted => ErrorKind::Interrupted,
            Pal::UnexpectedEof => ErrorKind::UnexpectedEof,
            Pal::Unsupported => ErrorKind::Unsupported,
            _ => ErrorKind::Other,
        };
        Self::new(kind, error.raw_os_error())
    }
}

#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_error::ErrorKind)]
pub enum ErrorKind {
    /// The peer is not listening (TCP RST in answer to SYN).
    #[error("connection refused")]
    ConnectionRefused,
    /// The peer reset the connection, or it was lost (keepalive or retransmission timeout).
    #[error("connection reset")]
    ConnectionReset,
    /// The local stack dropped the connection.
    #[error("connection aborted")]
    ConnectionAborted,
    #[error("not connected")]
    NotConnected,
    /// Sending after this side shut down writing, or after the peer closed.
    #[error("broken pipe")]
    BrokenPipe,
    #[error("address in use")]
    AddrInUse,
    #[error("address not available")]
    AddrNotAvailable,
    #[error("host unreachable")]
    HostUnreachable,
    #[error("network unreachable")]
    NetworkUnreachable,
    /// The operation was cancelled ([`NetQueue::cancel`](crate::NetQueue::cancel),
    /// [`NetQueue::close`](crate::NetQueue::close)).
    #[error("cancelled")]
    Cancelled,
    /// The peer closed the connection before the requested bytes arrived.
    #[error("unexpected end of stream")]
    UnexpectedEof,
    /// Name resolution found no such host.
    #[error("not found")]
    NotFound,
    #[error("timed out")]
    TimedOut,
    #[error("operation would block")]
    WouldBlock,
    #[error("interrupted")]
    Interrupted,
    #[error("invalid input")]
    InvalidInput,
    #[error("permission denied")]
    PermissionDenied,
    #[error("out of memory")]
    OutOfMemory,
    #[error("unsupported")]
    Unsupported,
    #[error("other error")]
    Other,
}
