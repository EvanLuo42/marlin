//! Zstandard (RFC 8878): a complete frame decoder and an encoder.
//!
//! The decoder handles every feature of the format except dictionaries: zstd and skippable
//! frames (concatenated), raw, RLE and compressed blocks, Huffman literals in 1 or 4 streams
//! (including treeless reuse), FSE sequence tables in predefined, RLE, compressed and repeat
//! modes, repeat offsets, and the XXH64 content checksum.
//!
//! The encoder writes single-segment frames with the content size: hash-chain matching
//! (greedy at level 1, lazy above), Huffman literals, and FSE sequences with per-block choice
//! between predefined, RLE and compressed tables.

mod bits;
mod block;
#[cfg(feature = "encode")]
mod enc;
mod fse;
mod huf;

#[cfg(feature = "encode")]
pub use enc::{FrameOptions, compress, max_compressed_size};

use crate::util::{le32, le64};
use crate::{Codec, Error, ErrorKind, Result};
use alloc::boxed::Box;
use block::Decoder;
use marlin_hash::xxh64;

pub(crate) const MAGIC: u32 = 0xFD2F_B528;
/// Skippable frames use `0x184D2A50..=0x184D2A5F`.
pub(crate) const SKIPPABLE_MAGIC: u32 = 0x184D_2A50;
/// `Block_Maximum_Size`.
pub(crate) const MAX_BLOCK: usize = 128 << 10;

#[inline]
pub(crate) fn corrupt(what: &'static str) -> Error {
    Error::new(Codec::Zstd, ErrorKind::Corrupt, what)
}

#[inline]
pub(crate) fn too_small() -> Error {
    Error::new(Codec::Zstd, ErrorKind::OutputTooSmall, "frame")
}

/// Decodes every frame in `src` (zstd frames and skippable frames, concatenated) into `dst`;
/// returns the total decoded size.
pub fn decompress(src: &[u8], dst: &mut [u8]) -> Result<usize> {
    if src.is_empty() {
        return Err(corrupt("no frame"));
    }
    let mut decoder: Option<Box<Decoder>> = None;
    let mut ip = 0;
    let mut op = 0;
    while ip < src.len() {
        let Some(magic) = le32(src, ip) else {
            return Err(corrupt("truncated magic"));
        };
        ip += 4;
        if magic & 0xFFFF_FFF0 == SKIPPABLE_MAGIC {
            let Some(size) = le32(src, ip) else {
                return Err(corrupt("truncated skippable frame"));
            };
            ip += 4;
            if size as usize > src.len() - ip {
                return Err(corrupt("truncated skippable frame"));
            }
            ip += size as usize;
            continue;
        }
        if magic != MAGIC {
            return Err(corrupt("bad magic"));
        }
        let d = decoder.get_or_insert_with(Decoder::new);
        op = decode_frame(d, src, &mut ip, dst, op)?;
    }
    Ok(op)
}

fn decode_frame(
    d: &mut Decoder,
    src: &[u8],
    ip: &mut usize,
    dst: &mut [u8],
    frame_start: usize,
) -> Result<usize> {
    let Some(&fhd) = src.get(*ip) else {
        return Err(corrupt("truncated frame header"));
    };
    *ip += 1;
    if fhd & 0x08 != 0 {
        return Err(corrupt("reserved frame header bit"));
    }
    let single_segment = fhd & 0x20 != 0;
    let has_checksum = fhd & 0x04 != 0;
    if !single_segment {
        // The window descriptor: decoding into `dst` needs no window, so only skip it.
        if src.get(*ip).is_none() {
            return Err(corrupt("truncated window descriptor"));
        }
        *ip += 1;
    }
    let dict_bytes = [0, 1, 2, 4][(fhd & 3) as usize];
    let Some(dict) = src.get(*ip..*ip + dict_bytes) else {
        return Err(corrupt("truncated dictionary id"));
    };
    *ip += dict_bytes;
    if dict.iter().any(|&b| b != 0) {
        return Err(Error::new(
            Codec::Zstd,
            ErrorKind::Unsupported,
            "dictionary",
        ));
    }
    let fcs_bytes = match fhd >> 6 {
        0 => usize::from(single_segment),
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let content_size = if fcs_bytes == 0 {
        None
    } else {
        let Some(field) = src.get(*ip..*ip + fcs_bytes) else {
            return Err(corrupt("truncated content size"));
        };
        *ip += fcs_bytes;
        let mut word = [0u8; 8];
        word[..fcs_bytes].copy_from_slice(field);
        let v = u64::from_le_bytes(word);
        Some(if fcs_bytes == 2 { v + 256 } else { v })
    };

    d.reset_frame();
    let mut op = frame_start;
    loop {
        let Some(h) = src.get(*ip..*ip + 3) else {
            return Err(corrupt("truncated block header"));
        };
        *ip += 3;
        let h = h[0] as u32 | (h[1] as u32) << 8 | (h[2] as u32) << 16;
        let last = h & 1 != 0;
        let size = (h >> 3) as usize;
        match (h >> 1) & 3 {
            0 => {
                if size > MAX_BLOCK {
                    return Err(corrupt("block too large"));
                }
                let Some(data) = src.get(*ip..*ip + size) else {
                    return Err(corrupt("truncated raw block"));
                };
                let Some(out) = dst.get_mut(op..op + size) else {
                    return Err(too_small());
                };
                out.copy_from_slice(data);
                *ip += size;
                op += size;
            }
            1 => {
                if size > MAX_BLOCK {
                    return Err(corrupt("block too large"));
                }
                let Some(&b) = src.get(*ip) else {
                    return Err(corrupt("truncated rle block"));
                };
                let Some(out) = dst.get_mut(op..op + size) else {
                    return Err(too_small());
                };
                out.fill(b);
                *ip += 1;
                op += size;
            }
            2 => {
                if size > MAX_BLOCK {
                    return Err(corrupt("block too large"));
                }
                let Some(data) = src.get(*ip..*ip + size) else {
                    return Err(corrupt("truncated compressed block"));
                };
                *ip += size;
                // At most `MAX_BLOCK` of output: decoding into no more enforces the limit.
                let room = (dst.len() - op).min(MAX_BLOCK);
                op = d
                    .decode_block(data, &mut dst[..op + room], op, frame_start)
                    .map_err(|e| {
                        if e.kind() == ErrorKind::OutputTooSmall && room == MAX_BLOCK {
                            corrupt("block decodes past maximum size")
                        } else {
                            e
                        }
                    })?;
            }
            _ => return Err(corrupt("reserved block type")),
        }
        if last {
            break;
        }
    }
    if has_checksum {
        let Some(sum) = le32(src, *ip) else {
            return Err(corrupt("truncated checksum"));
        };
        *ip += 4;
        if xxh64(&dst[frame_start..op], 0) as u32 != sum {
            return Err(corrupt("content checksum"));
        }
    }
    if let Some(size) = content_size
        && size != (op - frame_start) as u64
    {
        return Err(corrupt("content size mismatch"));
    }
    Ok(op)
}

/// The decoded size a frame header declares, if it does; for sizing buffers of `.zst` files.
/// Only the first frame is inspected.
pub fn frame_content_size(src: &[u8]) -> Option<u64> {
    if le32(src, 0)? != MAGIC {
        return None;
    }
    let fhd = *src.get(4)?;
    let single = fhd & 0x20 != 0;
    let at = 5 + usize::from(!single) + [0, 1, 2, 4][(fhd & 3) as usize];
    match fhd >> 6 {
        0 if single => src.get(at).map(|&b| b as u64),
        0 => None,
        1 => crate::util::le16(src, at).map(|v| v as u64 + 256),
        2 => le32(src, at).map(|v| v as u64),
        _ => le64(src, at),
    }
}

// Literal length and match length codes (RFC 8878 3.1.1.3.2.1.1): (baseline, extra bits).
#[rustfmt::skip]
pub(crate) const LL_CODES: [(u32, u8); 36] = [
    (0, 0), (1, 0), (2, 0), (3, 0), (4, 0), (5, 0), (6, 0), (7, 0),
    (8, 0), (9, 0), (10, 0), (11, 0), (12, 0), (13, 0), (14, 0), (15, 0),
    (16, 1), (18, 1), (20, 1), (22, 1), (24, 2), (28, 2), (32, 3), (40, 3),
    (48, 4), (64, 6), (128, 7), (256, 8), (512, 9), (1024, 10), (2048, 11), (4096, 12),
    (8192, 13), (16384, 14), (32768, 15), (65536, 16),
];

#[rustfmt::skip]
pub(crate) const ML_CODES: [(u32, u8); 53] = [
    (3, 0), (4, 0), (5, 0), (6, 0), (7, 0), (8, 0), (9, 0), (10, 0),
    (11, 0), (12, 0), (13, 0), (14, 0), (15, 0), (16, 0), (17, 0), (18, 0),
    (19, 0), (20, 0), (21, 0), (22, 0), (23, 0), (24, 0), (25, 0), (26, 0),
    (27, 0), (28, 0), (29, 0), (30, 0), (31, 0), (32, 0), (33, 0), (34, 0),
    (35, 1), (37, 1), (39, 1), (41, 1), (43, 2), (47, 2), (51, 3), (59, 3),
    (67, 4), (83, 4), (99, 5), (131, 7), (259, 8), (515, 9), (1027, 10), (2051, 11),
    (4099, 12), (8195, 13), (16387, 14), (32771, 15), (65539, 16),
];

/// Largest offset code: offsets are below 2^32.
pub(crate) const MAX_OF_CODE: usize = 31;

// Predefined distributions (3.1.1.3.2.2).
#[rustfmt::skip]
pub(crate) const LL_DEFAULT: [i16; 36] = [
    4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1,
    -1, -1, -1, -1,
];
pub(crate) const LL_DEFAULT_LOG: u32 = 6;

#[rustfmt::skip]
pub(crate) const ML_DEFAULT: [i16; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1,
    -1, -1, -1, -1, -1,
];
pub(crate) const ML_DEFAULT_LOG: u32 = 6;

#[rustfmt::skip]
pub(crate) const OF_DEFAULT: [i16; 29] = [
    1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
];
pub(crate) const OF_DEFAULT_LOG: u32 = 5;
