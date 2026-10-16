use core::fmt;

pub type Result<T, E = Error> = core::result::Result<T, E>;

/// A PAL failure: a portable [`ErrorKind`] plus the platform's raw error code.
///
/// `Copy` and allocation-free, so the allocator itself can return it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    OutOfMemory,
    InvalidInput,
    PermissionDenied,
    NotFound,
    AlreadyExists,
    WouldBlock,
    TimedOut,
    Interrupted,
    /// A read ended before the requested bytes arrived.
    UnexpectedEof,
    Unsupported,
    Other,
}

impl ErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OutOfMemory => "out of memory",
            Self::InvalidInput => "invalid input",
            Self::PermissionDenied => "permission denied",
            Self::NotFound => "not found",
            Self::AlreadyExists => "already exists",
            Self::WouldBlock => "operation would block",
            Self::TimedOut => "timed out",
            Self::Interrupted => "interrupted",
            Self::UnexpectedEof => "unexpected end of file",
            Self::Unsupported => "unsupported",
            Self::Other => "other error",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.raw == 0 {
            f.write_str(self.kind.as_str())
        } else {
            write!(f, "{} (os error {})", self.kind.as_str(), self.raw)
        }
    }
}

impl core::error::Error for Error {}
