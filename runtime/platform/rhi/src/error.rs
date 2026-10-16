use core::fmt;

pub type Result<T, E = Error> = core::result::Result<T, E>;

/// An RHI failure: a portable [`ErrorKind`] plus the API's raw code (`HRESULT`, `VkResult`, ...).
/// `Copy` and allocation-free, like `marlin_pal::Error`.
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

    #[inline]
    pub const fn from_kind(kind: ErrorKind) -> Self {
        Self { kind, raw: 0 }
    }

    #[inline]
    pub const fn kind(self) -> ErrorKind {
        self.kind
    }

    /// The API's own result code, or 0.
    #[inline]
    pub const fn raw(self) -> i32 {
        self.raw
    }
}

#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    /// The GPU was removed, reset or hung, or the driver was updated. Every object of the device
    /// is dead; recreate the device and everything made from it.
    DeviceLost,
    /// The OS, driver or GPU lacks something required (API version, feature level, format).
    Unsupported,
    OutOfMemory,
    /// No adapter matched the request.
    NotFound,
    InvalidInput,
    TimedOut,
    Other,
}

impl ErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DeviceLost => "device lost",
            Self::Unsupported => "unsupported",
            Self::OutOfMemory => "out of memory",
            Self::NotFound => "not found",
            Self::InvalidInput => "invalid input",
            Self::TimedOut => "timed out",
            Self::Other => "other error",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.raw == 0 {
            f.write_str(self.kind.as_str())
        } else {
            write!(f, "{} ({:#010x})", self.kind.as_str(), self.raw as u32)
        }
    }
}

impl core::error::Error for Error {}
