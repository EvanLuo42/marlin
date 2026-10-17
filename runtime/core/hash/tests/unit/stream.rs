//! Streaming XXH3 / XXH64 against the one-shot functions, over every way of splitting the input.

use std::hash::Hasher;

use marlin_hash::{Xxh3, Xxh64, xxh3_64_with_seed, xxh3_128_with_seed, xxh64};
use marlin_math::Rng;

/// Random bytes. Streaming is compared with one-shot, so any input does.
fn data(n: usize) -> Vec<u8> {
    let mut bytes = vec![0; n];
    Rng::new(0x9e37_79b9_7f4a_7c15).fill_bytes(&mut bytes);
    bytes
}

const SEEDS: [u64; 2] = [0, 0x9e37_79b9_7f4a_7c15];

/// Feeds `input` cut at `cuts` (ascending) and compares every digest with the one-shot hashes.
fn check(input: &[u8], cuts: &[usize]) {
    for seed in SEEDS {
        let mut x3 = Xxh3::with_seed(seed);
        let mut x64 = Xxh64::new(seed);
        let mut start = 0;
        for &cut in cuts.iter().chain([&input.len()]) {
            x3.update(&input[start..cut]);
            x64.update(&input[start..cut]);
            start = cut;
        }
        let len = input.len();
        assert_eq!(
            x3.digest(),
            xxh3_64_with_seed(input, seed),
            "xxh3_64 len {len} cuts {cuts:?} seed {seed:#x}"
        );
        assert_eq!(
            x3.digest128(),
            xxh3_128_with_seed(input, seed),
            "xxh3_128 len {len} cuts {cuts:?} seed {seed:#x}"
        );
        assert_eq!(
            x64.digest(),
            xxh64(input, seed),
            "xxh64 len {len} cuts {cuts:?} seed {seed:#x}"
        );
    }
}

#[test]
fn every_single_split_of_short_and_mid_inputs() {
    // Past the 240-byte short/long boundary, the 256-byte buffer and a few of its refills.
    let d = data(700);
    for len in 0..=d.len() {
        for cut in 0..=len {
            check(&d[..len], &[cut]);
        }
    }
}

#[test]
fn every_single_split_around_block_boundaries() {
    // 1024-byte blocks: the last stripe of a block, and inputs ending exactly on one.
    let d = data(2200);
    for len in [1023, 1024, 1025, 1088, 1089, 2047, 2048, 2049, 2112, 2113] {
        for cut in 0..=len {
            check(&d[..len], &[cut]);
        }
    }
}

#[test]
fn every_chunk_size() {
    let d = data(4200);
    for len in [241, 300, 1025, 4097, 4200] {
        for chunk in 1..=300 {
            let cuts: Vec<usize> = (chunk..len).step_by(chunk).collect();
            check(&d[..len], &cuts);
        }
    }
}

#[test]
fn every_pair_of_cuts_in_a_long_input() {
    // Two cuts, each anywhere: buffered tails followed by long runs and vice versa.
    let d = data(1100);
    let len = d.len();
    for a in (0..=len).step_by(7) {
        for b in (a..=len).step_by(13) {
            check(&d, &[a, b]);
        }
    }
}

#[test]
fn digest_does_not_consume() {
    let d = data(3000);
    let mut h = Xxh3::new();
    let mut h64 = Xxh64::default();
    for (i, chunk) in d.chunks(97).enumerate() {
        h.update(chunk);
        h64.update(chunk);
        let end = ((i + 1) * 97).min(d.len());
        assert_eq!(h.digest(), xxh3_64_with_seed(&d[..end], 0));
        assert_eq!(h.digest128(), xxh3_128_with_seed(&d[..end], 0));
        assert_eq!(h64.digest(), xxh64(&d[..end], 0));
    }
    h.reset();
    h64.reset();
    assert_eq!(h.digest(), xxh3_64_with_seed(b"", 0));
    assert_eq!(h64.digest(), xxh64(b"", 0));
}

#[test]
fn hasher_writes_integers_little_endian() {
    let mut h = Xxh3::new();
    h.write_u8(1);
    h.write_u16(0x0302);
    h.write_u32(0x0706_0504);
    h.write_u64(0x0f0e_0d0c_0b0a_0908);
    h.write_u128(0x1f1e_1d1c_1b1a_1918_1716_1514_1312_1110);
    h.write_usize(0x2726_2524_2322_2120);
    h.write_i32(-1);
    let mut bytes: Vec<u8> = (1..0x28).collect();
    bytes.extend([0xff; 4]);
    assert_eq!(h.finish(), xxh3_64_with_seed(&bytes, 0));

    let mut h = Xxh64::new(5);
    h.write_u64(0x0807_0605_0403_0201);
    h.write(b"!");
    assert_eq!(h.finish(), xxh64(b"\x01\x02\x03\x04\x05\x06\x07\x08!", 5));
}
