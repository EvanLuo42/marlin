//! LZ4: the block format and the frame format (v1.6.x of the LZ4 specifications).
//!
//! [`Codec::Lz4`] chunks are single blocks: the chunk header carries the
//! sizes, so the frame's headers and checksums would be redundant. The frame format is here for
//! interchange with the reference `lz4` tool and for data that must carry its own size and
//! checksum.

mod frame;

pub use frame::decompress_frame;
#[cfg(feature = "encode")]
pub use frame::{BlockSize, FrameOptions, compress_frame};

use crate::util::copy_match;
use crate::{Codec, Error, ErrorKind, Result};

#[inline]
pub(crate) fn corrupt(what: &'static str) -> Error {
    Error::new(Codec::Lz4, ErrorKind::Corrupt, what)
}

#[inline]
fn too_small() -> Error {
    Error::new(Codec::Lz4, ErrorKind::OutputTooSmall, "block")
}

/// The largest input an LZ4 block may encode (`LZ4_MAX_INPUT_SIZE`).
pub const MAX_BLOCK_INPUT: usize = 0x7E00_0000;

/// Decodes one LZ4 block into `dst`; returns the decoded size.
pub fn decompress_block(src: &[u8], dst: &mut [u8]) -> Result<usize> {
    decode_block_at(src, dst, 0, 0)
}

/// Decodes one block into `dst[pos..]`, with matches allowed to reach back to `dst[floor..]`
/// (linked frame blocks see the earlier blocks). Returns the end position.
pub(crate) fn decode_block_at(
    src: &[u8],
    dst: &mut [u8],
    pos: usize,
    floor: usize,
) -> Result<usize> {
    let mut ip = 0;
    let mut op = pos;
    loop {
        let Some(&token) = src.get(ip) else {
            return Err(corrupt("truncated sequence"));
        };
        ip += 1;
        let mut lit = (token >> 4) as usize;
        if lit == 15 {
            lit = lit.saturating_add(read_length(src, &mut ip)?);
        }
        let Some(lits) = src.get(ip..).and_then(|s| s.get(..lit)) else {
            return Err(corrupt("literals past end of block"));
        };
        let Some(out) = dst.get_mut(op..).and_then(|d| d.get_mut(..lit)) else {
            return Err(too_small());
        };
        out.copy_from_slice(lits);
        ip += lit;
        op += lit;
        if ip == src.len() {
            // The last sequence is literals only.
            return Ok(op);
        }

        let Some(offset) = crate::util::le16(src, ip) else {
            return Err(corrupt("truncated offset"));
        };
        ip += 2;
        let offset = offset as usize;
        if offset == 0 || offset > op - floor {
            return Err(corrupt("offset before start of output"));
        }
        let mut len = (token & 15) as usize + 4;
        if len == 19 {
            len = len.saturating_add(read_length(src, &mut ip)?);
        }
        if len > dst.len() - op {
            return Err(too_small());
        }
        copy_match(dst, op, offset, len);
        op += len;
    }
}

/// The 255-run length extension after a 15 nibble.
#[inline]
fn read_length(src: &[u8], ip: &mut usize) -> Result<usize> {
    let mut total = 0usize;
    loop {
        let Some(&b) = src.get(*ip) else {
            return Err(corrupt("truncated length"));
        };
        *ip += 1;
        total = total.saturating_add(b as usize);
        if b != 255 {
            return Ok(total);
        }
    }
}

#[cfg(feature = "encode")]
mod encode {
    use super::MAX_BLOCK_INPUT;
    use crate::matcher::{Matcher, Params, Step};
    use crate::{Codec, Error, ErrorKind, Level, Result};
    use alloc::vec::Vec;

    /// The block rules: the last 5 bytes are always literals (`LASTLITERALS`), and the last
    /// match starts at least 12 bytes before the end (`MFLIMIT`).
    const LAST_LITERALS: usize = 5;
    const MF_LIMIT: usize = 12;

    /// `LZ4_compressBound`.
    pub fn max_block_size(len: usize) -> usize {
        len + len / 255 + 16
    }

    /// Compresses `src` as one LZ4 block appended to `out`.
    ///
    /// Levels 1 to 3 use a single-entry hash table (the reference's fast mode), higher levels
    /// hash chains with lazy matching.
    pub fn compress_block(src: &[u8], level: Level, out: &mut Vec<u8>) -> Result<()> {
        if src.len() > MAX_BLOCK_INPUT {
            return Err(Error::new(
                Codec::Lz4,
                ErrorKind::Unsupported,
                "block over 2 GiB",
            ));
        }
        out.reserve(max_block_size(src.len()));
        let n = src.len();
        if n < MF_LIMIT + 1 {
            write_sequence(out, src, 0, 0);
            return Ok(());
        }
        let p = match level.get() {
            1 => params(12, 1, 0),
            2 => params(14, 1, 0),
            3 => params(16, 1, 0),
            4 => params(16, 4, 1),
            5 => params(16, 8, 1),
            6 => params(17, 16, 1),
            7 => params(17, 32, 2),
            8 => params(17, 64, 2),
            _ => params(17, 256, 2),
        };
        let mut m = Matcher::new(p, n);
        let mut rep = 0;
        m.parse(
            src,
            0..n,
            n - LAST_LITERALS,
            n - MF_LIMIT,
            &mut rep,
            |s: Step| write_sequence(out, &src[s.start..s.start + s.literals], s.offset, s.len),
        );
        Ok(())
    }

    fn params(hash_log: u32, depth: u32, lazy: u8) -> Params {
        Params {
            min_match: 4,
            max_match: usize::MAX,
            max_offset: 65535,
            depth,
            nice: if depth == 1 {
                4
            } else {
                64 + 16 * depth as usize
            },
            lazy,
            hash_log,
        }
    }

    /// One sequence: literals, then a match unless `len == 0` (the last sequence).
    fn write_sequence(out: &mut Vec<u8>, literals: &[u8], offset: usize, len: usize) {
        let lit = literals.len();
        let ml = len.saturating_sub(4);
        let token = ((lit.min(15) as u8) << 4) | if len == 0 { 0 } else { ml.min(15) as u8 };
        out.push(token);
        if lit >= 15 {
            write_length(out, lit - 15);
        }
        out.extend_from_slice(literals);
        if len != 0 {
            out.extend_from_slice(&(offset as u16).to_le_bytes());
            if ml >= 15 {
                write_length(out, ml - 15);
            }
        }
    }

    fn write_length(out: &mut Vec<u8>, mut n: usize) {
        while n >= 255 {
            out.push(255);
            n -= 255;
        }
        out.push(n as u8);
    }
}

#[cfg(feature = "encode")]
pub use encode::{compress_block, max_block_size};
