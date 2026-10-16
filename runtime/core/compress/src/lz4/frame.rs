//! The LZ4 frame format: magic, frame descriptor, blocks, end mark, optional checksums.

use super::{corrupt, decode_block_at};
use crate::util::{le32, le64};
use crate::{Codec, Error, ErrorKind, Result};
use marlin_hash::xxh32;

const MAGIC: u32 = 0x184D_2204;
const LEGACY_MAGIC: u32 = 0x184C_2102;
/// Skippable frames use `0x184D2A50..=0x184D2A5F`.
const SKIPPABLE_MAGIC: u32 = 0x184D_2A50;

const FLG_VERSION: u8 = 0b0100_0000;
const FLG_INDEPENDENT: u8 = 1 << 5;
const FLG_BLOCK_CHECKSUM: u8 = 1 << 4;
const FLG_CONTENT_SIZE: u8 = 1 << 3;
const FLG_CONTENT_CHECKSUM: u8 = 1 << 2;
const FLG_DICT_ID: u8 = 1 << 0;

const UNCOMPRESSED_BIT: u32 = 1 << 31;

/// Decodes every frame in `src` (LZ4 frames and skippable frames, concatenated) into `dst`;
/// returns the total decoded size. Legacy frames and frames that need a dictionary are
/// [`ErrorKind::Unsupported`].
pub fn decompress_frame(src: &[u8], dst: &mut [u8]) -> Result<usize> {
    if src.is_empty() {
        return Err(corrupt("no frame"));
    }
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
        if magic == LEGACY_MAGIC {
            return Err(Error::new(
                Codec::Lz4,
                ErrorKind::Unsupported,
                "legacy frame",
            ));
        }
        if magic != MAGIC {
            return Err(corrupt("bad magic"));
        }
        op = decode_frame(src, &mut ip, dst, op)?;
    }
    Ok(op)
}

fn decode_frame(src: &[u8], ip: &mut usize, dst: &mut [u8], frame_start: usize) -> Result<usize> {
    let desc_start = *ip;
    let (Some(&flg), Some(&bd)) = (src.get(*ip), src.get(*ip + 1)) else {
        return Err(corrupt("truncated frame descriptor"));
    };
    *ip += 2;
    if flg & 0b1100_0000 != FLG_VERSION || flg & 0b10 != 0 || bd & 0b1000_1111 != 0 {
        return Err(corrupt("bad frame descriptor"));
    }
    let block_max = match (bd >> 4) & 7 {
        4 => 64 << 10,
        5 => 256 << 10,
        6 => 1 << 20,
        7 => 4 << 20,
        _ => return Err(corrupt("bad block maximum size")),
    };
    let mut content_size = None;
    if flg & FLG_CONTENT_SIZE != 0 {
        let Some(size) = le64(src, *ip) else {
            return Err(corrupt("truncated content size"));
        };
        content_size = Some(size);
        *ip += 8;
    }
    if flg & FLG_DICT_ID != 0 {
        return Err(Error::new(Codec::Lz4, ErrorKind::Unsupported, "dictionary"));
    }
    let Some(&hc) = src.get(*ip) else {
        return Err(corrupt("truncated header checksum"));
    };
    if (xxh32(&src[desc_start..*ip], 0) >> 8) as u8 != hc {
        return Err(corrupt("header checksum"));
    }
    *ip += 1;

    let independent = flg & FLG_INDEPENDENT != 0;
    let block_checksum = flg & FLG_BLOCK_CHECKSUM != 0;
    let mut op = frame_start;
    loop {
        let Some(word) = le32(src, *ip) else {
            return Err(corrupt("truncated block size"));
        };
        *ip += 4;
        if word == 0 {
            break;
        }
        let size = (word & !UNCOMPRESSED_BIT) as usize;
        if size > block_max {
            return Err(corrupt("block larger than maximum"));
        }
        let Some(data) = src.get(*ip..).and_then(|s| s.get(..size)) else {
            return Err(corrupt("truncated block"));
        };
        *ip += size;
        if block_checksum {
            let Some(sum) = le32(src, *ip) else {
                return Err(corrupt("truncated block checksum"));
            };
            *ip += 4;
            if xxh32(data, 0) != sum {
                return Err(corrupt("block checksum"));
            }
        }
        // Decoding into at most `block_max` bytes enforces the frame's block size limit.
        let room = (dst.len() - op).min(block_max);
        if word & UNCOMPRESSED_BIT != 0 {
            if size > dst.len() - op {
                return Err(Error::new(Codec::Lz4, ErrorKind::OutputTooSmall, "frame"));
            }
            dst[op..op + size].copy_from_slice(data);
            op += size;
        } else {
            let floor = if independent { op } else { frame_start };
            op = decode_block_at(data, &mut dst[..op + room], op, floor).map_err(|e| {
                if e.kind() == ErrorKind::OutputTooSmall && room == block_max {
                    corrupt("block larger than maximum")
                } else {
                    e
                }
            })?;
        }
    }
    if flg & FLG_CONTENT_CHECKSUM != 0 {
        let Some(sum) = le32(src, *ip) else {
            return Err(corrupt("truncated content checksum"));
        };
        *ip += 4;
        if xxh32(&dst[frame_start..op], 0) != sum {
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

#[cfg(feature = "encode")]
mod encode {
    use super::*;
    use crate::Level;
    use alloc::vec::Vec;

    /// The maximum (uncompressed) size of a frame's blocks.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum BlockSize {
        Max64K = 4,
        Max256K = 5,
        Max1M = 6,
        Max4M = 7,
    }

    impl BlockSize {
        pub const fn bytes(self) -> usize {
            1 << (8 + 2 * self as u32)
        }
    }

    /// Frame settings. The defaults match the reference `lz4` tool: 4 MiB blocks and a content
    /// checksum. Blocks are always independent.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct FrameOptions {
        pub block_size: BlockSize,
        pub block_checksum: bool,
        pub content_checksum: bool,
        /// Store the content size in the header.
        pub content_size: bool,
    }

    impl Default for FrameOptions {
        fn default() -> Self {
            Self {
                block_size: BlockSize::Max4M,
                block_checksum: false,
                content_checksum: true,
                content_size: false,
            }
        }
    }

    /// Compresses `src` as one LZ4 frame appended to `out`. Blocks that do not shrink are
    /// stored uncompressed.
    pub fn compress_frame(
        src: &[u8],
        level: Level,
        opts: &FrameOptions,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        out.extend_from_slice(&MAGIC.to_le_bytes());
        let desc_start = out.len();
        let mut flg = FLG_VERSION | FLG_INDEPENDENT;
        if opts.block_checksum {
            flg |= FLG_BLOCK_CHECKSUM;
        }
        if opts.content_size {
            flg |= FLG_CONTENT_SIZE;
        }
        if opts.content_checksum {
            flg |= FLG_CONTENT_CHECKSUM;
        }
        out.push(flg);
        out.push((opts.block_size as u8) << 4);
        if opts.content_size {
            out.extend_from_slice(&(src.len() as u64).to_le_bytes());
        }
        let hc = (xxh32(&out[desc_start..], 0) >> 8) as u8;
        out.push(hc);

        let mut block = Vec::new();
        for chunk in src.chunks(opts.block_size.bytes()) {
            block.clear();
            super::super::compress_block(chunk, level, &mut block)?;
            let (word, data) = if block.len() < chunk.len() {
                (block.len() as u32, &block[..])
            } else {
                (chunk.len() as u32 | UNCOMPRESSED_BIT, chunk)
            };
            out.extend_from_slice(&word.to_le_bytes());
            out.extend_from_slice(data);
            if opts.block_checksum {
                out.extend_from_slice(&xxh32(data, 0).to_le_bytes());
            }
        }
        out.extend_from_slice(&0u32.to_le_bytes());
        if opts.content_checksum {
            out.extend_from_slice(&xxh32(src, 0).to_le_bytes());
        }
        Ok(())
    }
}

#[cfg(feature = "encode")]
pub use encode::{BlockSize, FrameOptions, compress_frame};
