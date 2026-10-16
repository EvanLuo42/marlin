use crate::Codec;

pub type Result<T, E = Error> = core::result::Result<T, E>;

/// A codec failure: what went wrong ([`ErrorKind`]), in which codec, and a short static note on
/// where, for debugging bad data. `Copy` and allocation-free.
#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_std::Error)]
#[error("{codec}: {kind}: {what}")]
pub struct Error {
    codec: Codec,
    kind: ErrorKind,
    what: &'static str,
}

impl Error {
    #[inline]
    pub const fn new(codec: Codec, kind: ErrorKind, what: &'static str) -> Self {
        Self { codec, kind, what }
    }

    #[inline]
    pub const fn kind(self) -> ErrorKind {
        self.kind
    }

    #[inline]
    pub const fn codec(self) -> Codec {
        self.codec
    }

    /// Which check failed, e.g. `"huffman table"`. For logs only; the wording may change.
    #[inline]
    pub const fn what(self) -> &'static str {
        self.what
    }
}

#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_std::ErrorKind)]
pub enum ErrorKind {
    /// The input is not valid data of the codec: damaged, truncated or not that format.
    #[error("corrupt data")]
    Corrupt,
    /// The decoded data does not fit the destination.
    #[error("output too small")]
    OutputTooSmall,
    /// Valid data using a feature this implementation lacks (zstd dictionaries, LZ4 legacy
    /// frames), or input too large for the format when encoding.
    #[error("unsupported")]
    Unsupported,
}
