pub type Result<T, E = Error> = core::result::Result<T, E>;

/// A PAL failure: a portable [`ErrorKind`] plus the platform's raw error code.
///
/// `Copy` and allocation-free, so the allocator itself can return it.
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

    /// An error detected by the PAL itself rather than reported by the OS.
    #[inline]
    pub const fn from_kind(kind: ErrorKind) -> Self {
        Self { kind, raw: 0 }
    }

    #[inline]
    pub const fn kind(self) -> ErrorKind {
        self.kind
    }

    /// The platform error code (`GetLastError`, `errno`, console SDK result), or 0.
    #[inline]
    pub const fn raw_os_error(self) -> i32 {
        self.raw
    }
}

#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_error::ErrorKind)]
pub enum ErrorKind {
    #[error("out of memory")]
    OutOfMemory,
    #[error("invalid input")]
    InvalidInput,
    #[error("permission denied")]
    PermissionDenied,
    #[error("not found")]
    NotFound,
    #[error("already exists")]
    AlreadyExists,
    #[error("operation would block")]
    WouldBlock,
    #[error("timed out")]
    TimedOut,
    #[error("interrupted")]
    Interrupted,
    /// A read ended before the requested bytes arrived.
    #[error("unexpected end of file")]
    UnexpectedEof,
    /// Data from the OS was malformed, e.g. a path that is not valid Unicode.
    #[error("invalid data")]
    InvalidData,
    /// The operation was cancelled before it finished.
    #[error("cancelled")]
    Cancelled,
    #[error("directory not empty")]
    DirectoryNotEmpty,
    #[error("not a directory")]
    NotADirectory,
    #[error("is a directory")]
    IsADirectory,
    #[error("unsupported")]
    Unsupported,
    #[error("other error")]
    Other,
}
