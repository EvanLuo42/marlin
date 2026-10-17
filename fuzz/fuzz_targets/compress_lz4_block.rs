//! `marlin_compress::lz4::decompress_block` (`decompress(Codec::Lz4, ..)`, the package chunk
//! path) on any bytes.
//!
//! Input: a 3-byte little-endian destination size (taken modulo 1 MiB + 1), then the block.
#![no_main]

use marlin_compress::{Codec, decompress};

fn run(data: &[u8]) {
    let Some(([a, b, c], block)) = marlin_fuzz::split::<3>(data) else {
        return;
    };
    let len = u32::from_le_bytes([a, b, c, 0]) as usize % ((1 << 20) + 1);
    let mut dst = vec![0u8; len];
    let _ = decompress(Codec::Lz4, block, &mut dst);
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
