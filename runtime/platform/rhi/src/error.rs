pub type Result<T, E = Error> = core::result::Result<T, E>;

/// An RHI failure: a portable [`ErrorKind`] plus the API's raw code (`HRESULT`, `VkResult`, ...).
/// `Copy` and allocation-free, like `marlin_pal::Error`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_error::Error)]
#[error(code = "{:#010x}")]
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
#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_error::ErrorKind)]
pub enum ErrorKind {
    /// The GPU was removed, reset or hung, or the driver was updated. Every object of the device
    /// is dead; recreate the device and everything made from it.
    #[error("device lost")]
    DeviceLost,
    /// The OS, driver or GPU lacks something required (API version, feature level, format).
    #[error("unsupported")]
    Unsupported,
    #[error("out of memory")]
    OutOfMemory,
    /// No adapter matched the request.
    #[error("not found")]
    NotFound,
    #[error("invalid input")]
    InvalidInput,
    #[error("timed out")]
    TimedOut,
    #[error("other error")]
    Other,
}
