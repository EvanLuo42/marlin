//! The runtime SIMD path of XXH3 against the forced-scalar path, which shares its code with the
//! const evaluation pinned by the reference vectors in `stable.rs`.

use std::time::Instant;

use marlin_hash::stable::scalar;
use marlin_hash::{xxh3_64_ascii_lowercase, xxh3_64_with_seed, xxh3_128_with_seed};
use marlin_math::Rng;

/// Deterministic bytes, mixed-case ASCII and high bytes included so the lowercase variant matters.
fn data(n: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    (0..n).map(|_| (rng.next_u64() >> 24) as u8).collect()
}

#[test]
fn every_length_matches_scalar() {
    // Covers the short paths, every stripe count of the tail, and several 1 KiB block boundaries.
    let d = data(4200, 1);
    for len in 0..=d.len() {
        let input = &d[..len];
        for seed in [0, 42, u64::MAX] {
            assert_eq!(
                xxh3_64_with_seed(input, seed),
                scalar::xxh3_64_with_seed(input, seed),
                "64-bit, len {len}, seed {seed}"
            );
            assert_eq!(
                xxh3_128_with_seed(input, seed),
                scalar::xxh3_128_with_seed(input, seed),
                "128-bit, len {len}, seed {seed}"
            );
        }
        assert_eq!(
            xxh3_64_ascii_lowercase(input),
            scalar::xxh3_64_ascii_lowercase(input),
            "lowercase, len {len}"
        );
    }
}

#[test]
fn unaligned_and_large_inputs_match_scalar() {
    let d = data((1 << 20) + 64, 7);
    for offset in 0..64 {
        let input = &d[offset..offset + 5000];
        assert_eq!(
            xxh3_64_with_seed(input, 3),
            scalar::xxh3_64_with_seed(input, 3)
        );
    }
    let big = &d[3..];
    assert_eq!(xxh3_64_with_seed(big, 0), scalar::xxh3_64_with_seed(big, 0));
    assert_eq!(
        xxh3_128_with_seed(big, 9),
        scalar::xxh3_128_with_seed(big, 9)
    );
    assert_eq!(
        xxh3_64_ascii_lowercase(big),
        scalar::xxh3_64_ascii_lowercase(big)
    );
}

/// `cargo test --release -p marlin-hash --test simd -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_throughput() {
    fn gbps(bytes: usize, f: impl Fn() -> u64) -> f64 {
        // Enough repetitions for ~256 MiB per measurement; best of three runs.
        let reps = ((256 << 20) / bytes).max(1);
        (0..3)
            .map(|_| {
                let start = Instant::now();
                let mut sink = 0;
                for _ in 0..reps {
                    sink ^= f();
                }
                std::hint::black_box(sink);
                (bytes * reps) as f64 / start.elapsed().as_secs_f64() / 1e9
            })
            .fold(0.0, f64::max)
    }

    println!(
        "{:>10} {:>12} {:>12} {:>8}",
        "size", "scalar GB/s", "simd GB/s", "speedup"
    );
    for size in [256, 1 << 10, 4 << 10, 64 << 10, 1 << 20, 16 << 20] {
        let d = data(size, 3);
        let scalar = gbps(size, || {
            scalar::xxh3_64_with_seed(std::hint::black_box(&d), 0)
        });
        let simd = gbps(size, || xxh3_64_with_seed(std::hint::black_box(&d), 0));
        println!(
            "{size:>10} {scalar:>12.2} {simd:>12.2} {:>7.2}x",
            simd / scalar
        );
    }
}
