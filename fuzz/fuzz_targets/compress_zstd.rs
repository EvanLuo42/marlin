//! `marlin_compress::zstd::decompress` (also `decompress(Codec::Zstd, ..)`, the package chunk
//! path) on any bytes, into a buffer of the declared content size and into a fixed one.
#![no_main]

use marlin_compress::{Codec, decompress, zstd};

/// Largest destination the target allocates: bigger declared sizes use [`FIXED`].
const MAX_DST: u64 = 8 << 20;
const FIXED: usize = 512 << 10;

fn run(data: &[u8]) {
    let declared = zstd::frame_content_size(data).filter(|&n| n <= MAX_DST);
    let mut dst = vec![0u8; declared.map_or(FIXED, |n| n as usize)];
    let _ = decompress(Codec::Zstd, data, &mut dst);
    if declared.is_some() {
        let mut fixed = vec![0u8; FIXED];
        let _ = zstd::decompress(data, &mut fixed);
    }
    // A destination one byte short of the declared size: `OutputTooSmall`, never a write past it.
    if let Some(n) = declared.filter(|&n| n > 0) {
        let _ = zstd::decompress(data, &mut dst[..n as usize - 1]);
    }
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
