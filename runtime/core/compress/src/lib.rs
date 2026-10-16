//! In-house zstd, LZ4 and GDeflate: the codecs of package (`.mpak`) chunks.
//!
//! Every format is implemented here from its specification; there is no third-party
//! compression code in the engine. Decoders are always built and run in the shipping no_std
//! runtime. Encoders are for cooking tools and sit behind the `encode` feature.
//!
//! # Block API
//!
//! A package chunk records its [`Codec`] and both of its sizes, so the core API is one call per
//! chunk with the whole input and a destination of the known size:
//!
//! ```ignore
//! let n = marlin_compress::decompress(chunk.codec, &chunk.bytes, &mut dst[..chunk.raw_len])?;
//! debug_assert_eq!(n, chunk.raw_len);
//!
//! // Tools (`encode` feature):
//! let mut out = Vec::with_capacity(marlin_compress::max_compressed_size(Codec::Zstd, src.len()));
//! marlin_compress::compress(Codec::Zstd, Level::DEFAULT, &src, &mut out)?;
//! ```
//!
//! What each codec's payload is:
//!
//! | [`Codec`]           | payload                                                            |
//! |---------------------|--------------------------------------------------------------------|
//! | [`Codec::None`]     | the bytes themselves                                               |
//! | [`Codec::Lz4`]      | one LZ4 *block* (no frame: the chunk header already has the sizes) |
//! | [`Codec::Zstd`]     | zstd frames (RFC 8878), as written by the reference `zstd`         |
//! | [`Codec::GDeflate`] | a DirectStorage GDeflate tile stream (64 KiB tiles)                |
//!
//! The codec modules ([`lz4`], [`zstd`], [`gdeflate`]) expose the format-level calls: LZ4
//! frames, zstd frame options, GDeflate tiles.
//!
//! # Decoder guarantees
//!
//! Decoders treat their input as hostile. On any input, corrupt, truncated or malicious, they
//! never panic, never read or write out of bounds, and finish in time linear in
//! `src.len() + dst.len()`; bad input yields [`ErrorKind::Corrupt`]. They write only into
//! `dst[..n]` on success; on error `dst` holds unspecified bytes. Decoders allocate a small
//! fixed workspace per call (tables; for zstd also the 128 KiB literal buffer) and never more.
//!
//! # Room to grow
//!
//! - Streaming: decoders already decode into `dst` with history = `dst[..pos]`; a streaming
//!   front end adds a ring-buffer window and resumable state on top of the same block decoders.
//! - zstd dictionaries: frames with a dictionary ID are rejected with [`ErrorKind::Unsupported`]
//!   today; a dictionary would seed the history, entropy tables and repeat offsets.
//! - Hardware decompression (console decompression units, DirectStorage GPU GDeflate) lives in
//!   an L1 `ext`, not here; the payload formats above are the ones that hardware consumes, and
//!   these decoders are the CPU fallback.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod error;
pub mod gdeflate;
pub mod lz4;
mod util;
pub mod zstd;

#[cfg(feature = "encode")]
mod huffman;
#[cfg(feature = "encode")]
mod matcher;

use core::fmt;

pub use error::{Error, ErrorKind, Result};

/// The compression of one chunk. The discriminants are the on-disk IDs ([`Codec::id`]); they
/// never change.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum Codec {
    /// Stored as is.
    None = 0,
    /// LZ4 block format: the fastest decoder, a modest ratio.
    Lz4 = 1,
    /// Zstandard: the best ratio, still fast to decode.
    Zstd = 2,
    /// GDeflate: Deflate split into 32 interleaved bit streams per 64 KiB tile, so a GPU (or
    /// SIMD) can decode a tile in parallel. The DirectStorage GPU decompression format.
    GDeflate = 3,
}

impl Codec {
    pub const ALL: [Codec; 4] = [Codec::None, Codec::Lz4, Codec::Zstd, Codec::GDeflate];

    /// The on-disk ID.
    #[inline]
    pub const fn id(self) -> u8 {
        self as u8
    }

    /// The codec with on-disk ID `id`, if there is one.
    #[inline]
    pub const fn from_id(id: u8) -> Option<Codec> {
        match id {
            0 => Some(Codec::None),
            1 => Some(Codec::Lz4),
            2 => Some(Codec::Zstd),
            3 => Some(Codec::GDeflate),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Codec::None => "none",
            Codec::Lz4 => "lz4",
            Codec::Zstd => "zstd",
            Codec::GDeflate => "gdeflate",
        }
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Decodes the whole of `src` into `dst` and returns the number of bytes written.
///
/// `dst` is normally exactly the uncompressed size from the chunk header; a larger `dst` is
/// fine and the return value says how much was used. Fails with [`ErrorKind::OutputTooSmall`]
/// if the data does not fit, [`ErrorKind::Corrupt`] on bad input and
/// [`ErrorKind::Unsupported`] for valid input using a feature not implemented here (zstd
/// dictionaries).
pub fn decompress(codec: Codec, src: &[u8], dst: &mut [u8]) -> Result<usize> {
    match codec {
        Codec::None => {
            let Some(out) = dst.get_mut(..src.len()) else {
                return Err(Error::new(codec, ErrorKind::OutputTooSmall, "stored chunk"));
            };
            out.copy_from_slice(src);
            Ok(src.len())
        }
        Codec::Lz4 => lz4::decompress_block(src, dst),
        Codec::Zstd => zstd::decompress(src, dst),
        Codec::GDeflate => gdeflate::decompress(src, dst),
    }
}

/// Encoder effort, 1 (fastest) to 9 (smallest); [`Level::DEFAULT`] is 5. The scale is per codec:
/// the same level does not mean the same speed or ratio across codecs. Out-of-range values
/// clamp.
#[cfg(feature = "encode")]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Level(u8);

#[cfg(feature = "encode")]
impl Level {
    pub const FASTEST: Level = Level(1);
    pub const DEFAULT: Level = Level(5);
    pub const BEST: Level = Level(9);

    pub const fn new(level: u8) -> Level {
        Level(if level < 1 {
            1
        } else if level > 9 {
            9
        } else {
            level
        })
    }

    #[inline]
    pub const fn get(self) -> u8 {
        self.0
    }
}

#[cfg(feature = "encode")]
impl Default for Level {
    fn default() -> Level {
        Level::DEFAULT
    }
}

/// Compresses `src` and appends the payload to `out`; returns the number of bytes appended.
///
/// The output never exceeds [`max_compressed_size`]: data that does not compress is stored in
/// the format's own uncompressed form. Fails only with [`ErrorKind::Unsupported`] when `src` is
/// larger than the format can describe (4 GiB - 64 KiB for GDeflate, about 2 GiB for an LZ4
/// block).
#[cfg(feature = "encode")]
pub fn compress(
    codec: Codec,
    level: Level,
    src: &[u8],
    out: &mut alloc::vec::Vec<u8>,
) -> Result<usize> {
    let start = out.len();
    match codec {
        Codec::None => out.extend_from_slice(src),
        Codec::Lz4 => lz4::compress_block(src, level, out)?,
        Codec::Zstd => zstd::compress(src, level, &zstd::FrameOptions::default(), out)?,
        Codec::GDeflate => gdeflate::compress(src, level, out)?,
    }
    Ok(out.len() - start)
}

/// An upper bound on the payload [`compress`] produces for `len` input bytes, for sizing
/// output buffers.
#[cfg(feature = "encode")]
pub fn max_compressed_size(codec: Codec, len: usize) -> usize {
    match codec {
        Codec::None => len,
        Codec::Lz4 => lz4::max_block_size(len),
        Codec::Zstd => zstd::max_compressed_size(len),
        Codec::GDeflate => gdeflate::max_compressed_size(len),
    }
}
