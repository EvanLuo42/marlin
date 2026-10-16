//! GDeflate: Deflate re-laid-out for parallel decoding, as specified by Microsoft's DirectStorage
//! (`GDeflate/` in github.com/microsoft/DirectStorage, built on NVIDIA's libdeflate fork).
//!
//! The symbols are Deflate64's, not RFC 1951's: length symbol 285 carries 16 extra bits
//! (lengths up to 65538) and distance symbols 30 and 31 reach 64 KiB back.
//!
//! # Tile stream
//!
//! The input is cut into 64 KiB tiles, each compressed independently:
//!
//! | bytes          | field                                                              |
//! |----------------|--------------------------------------------------------------------|
//! | 0              | codec ID, 4                                                        |
//! | 1              | magic, `ID ^ 0xFF`                                                 |
//! | 2..4           | tile count, u16 LE                                                 |
//! | 4..8           | u32 LE: bits 0-1 tile size index (1 = 64 KiB), bits 2-19 size of the last tile (0 = a full tile), bits 20-31 reserved |
//! | 8..8 + 4n      | u32 LE per tile: entry 0 is the compressed size of the *last* tile, entry `i > 0` the offset of tile `i` from the end of this table |
//! | ...            | tile data                                                          |
//!
//! # Tile bit stream
//!
//! A tile is Deflate blocks whose bits are dealt round-robin to 32 streams: symbol `k` of a
//! block goes to stream `k % 32` (block headers to stream 0), so 32 GPU lanes each decode their
//! own stream. A stream refills from the shared input 32 bits at a time, in the order the
//! decoder runs dry. A match's offset is not coded right after its length but on the same
//! stream's next turn, so the lanes never wait on each other: the copy is deferred, and run when
//! the offset arrives. Stored blocks have no `NLEN` and no byte alignment.
//!
//! The decoder here is the CPU path: it runs the 32 streams in turn, exactly mirroring the
//! reference decoder's refill points, which define the format.

mod decode;
#[cfg(feature = "encode")]
mod encode;

pub use decode::decompress_tile;
#[cfg(feature = "encode")]
pub use encode::{compress, compress_tile, max_compressed_size};

use crate::util::le32;
use crate::{Codec, Error, ErrorKind, Result};
use core::ops::Range;

/// Uncompressed size of every tile but the last.
pub const TILE_SIZE: usize = 64 << 10;
/// The most tiles a stream can hold.
pub const MAX_TILES: usize = 0xFFFF;
const ID: u8 = 4;
const HEADER: usize = 8;

#[inline]
pub(crate) fn corrupt(what: &'static str) -> Error {
    Error::new(Codec::GDeflate, ErrorKind::Corrupt, what)
}

/// The tiles of a GDeflate stream, with the header validated: for decoding tiles in parallel.
pub struct Tiles<'a> {
    offsets: &'a [u8],
    data: &'a [u8],
    count: usize,
    last_size: usize,
}

impl<'a> Tiles<'a> {
    pub fn parse(src: &'a [u8]) -> Result<Tiles<'a>> {
        let Some(&[id, magic, n0, n1]) = src.first_chunk::<4>() else {
            return Err(corrupt("truncated header"));
        };
        if id != ID || magic != ID ^ 0xFF {
            return Err(corrupt("bad magic"));
        }
        let Some(word) = le32(src, 4) else {
            return Err(corrupt("truncated header"));
        };
        if word & 3 != 1 {
            return Err(Error::new(
                Codec::GDeflate,
                ErrorKind::Unsupported,
                "tile size other than 64 KiB",
            ));
        }
        let count = u16::from_le_bytes([n0, n1]) as usize;
        let last = ((word >> 2) & 0x3_FFFF) as usize;
        if last > TILE_SIZE || (count == 0 && last != 0) {
            return Err(corrupt("bad last tile size"));
        }
        let Some(offsets) = src.get(HEADER..HEADER + 4 * count) else {
            return Err(corrupt("truncated tile table"));
        };
        let tiles = Tiles {
            offsets,
            data: &src[HEADER + 4 * count..],
            count,
            last_size: if last == 0 { TILE_SIZE } else { last },
        };
        // Validate every range once, so `get` cannot fail.
        let mut prev = 0;
        for i in 0..count {
            let r = tiles.compressed_range(i);
            if r.start < prev || r.start > r.end || r.end > tiles.data.len() {
                return Err(corrupt("bad tile offsets"));
            }
            prev = r.start;
        }
        Ok(tiles)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.count
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Total decoded size.
    pub fn uncompressed_size(&self) -> usize {
        match self.count {
            0 => 0,
            n => (n - 1) * TILE_SIZE + self.last_size,
        }
    }

    fn word(&self, i: usize) -> usize {
        u32::from_le_bytes(self.offsets[4 * i..4 * i + 4].try_into().unwrap()) as usize
    }

    fn compressed_range(&self, i: usize) -> Range<usize> {
        let start = if i == 0 { 0 } else { self.word(i) };
        let end = if i + 1 < self.count {
            self.word(i + 1)
        } else {
            start.saturating_add(self.word(0))
        };
        start..end
    }

    /// Tile `i`'s compressed bytes and where its output goes in the decoded data.
    pub fn get(&self, i: usize) -> (&'a [u8], Range<usize>) {
        let r = self.compressed_range(i);
        let size = if i + 1 == self.count {
            self.last_size
        } else {
            TILE_SIZE
        };
        (&self.data[r], i * TILE_SIZE..i * TILE_SIZE + size)
    }
}

/// Decodes a whole tile stream into `dst`; returns the decoded size.
pub fn decompress(src: &[u8], dst: &mut [u8]) -> Result<usize> {
    let tiles = Tiles::parse(src)?;
    let total = tiles.uncompressed_size();
    if total > dst.len() {
        return Err(Error::new(
            Codec::GDeflate,
            ErrorKind::OutputTooSmall,
            "stream",
        ));
    }
    for i in 0..tiles.len() {
        let (data, out) = tiles.get(i);
        let len = out.len();
        let n = decompress_tile(data, &mut dst[out]).map_err(|e| {
            if e.kind() == ErrorKind::OutputTooSmall {
                corrupt("tile longer than declared")
            } else {
                e
            }
        })?;
        if n != len {
            return Err(corrupt("tile shorter than declared"));
        }
    }
    Ok(total)
}
