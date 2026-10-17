//! `marlin_compress::lz4::decompress_frame` on any bytes: LZ4 frames and skippable frames.
#![no_main]

use marlin_compress::lz4;

fn run(data: &[u8]) {
    let mut dst = vec![0u8; 1 << 20];
    let _ = lz4::decompress_frame(data, &mut dst);
    let _ = lz4::decompress_frame(data, &mut dst[..data.len().min(1 << 20) / 2]);
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
