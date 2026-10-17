//! `marlin_compress::gdeflate`: tile stream parsing, whole-stream decoding
//! (`decompress(Codec::GDeflate, ..)`, the package chunk path) and tile-by-tile decoding.
#![no_main]

use marlin_compress::{Codec, decompress, gdeflate};

/// Largest destination the target allocates.
const MAX_DST: usize = 16 << 20;

fn run(data: &[u8]) {
    let Ok(tiles) = gdeflate::Tiles::parse(data) else {
        let mut dst = vec![0u8; 1 << 16];
        let _ = decompress(Codec::GDeflate, data, &mut dst);
        return;
    };
    let size = tiles.uncompressed_size();
    if size > MAX_DST {
        return;
    }
    let mut dst = vec![0u8; size];
    let _ = decompress(Codec::GDeflate, data, &mut dst);
    for i in 0..tiles.len() {
        let (tile, range) = tiles.get(i);
        if let Some(out) = dst.get_mut(range) {
            let _ = gdeflate::decompress_tile(tile, out);
        }
    }
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
