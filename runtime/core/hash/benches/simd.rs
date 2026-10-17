//! XXH3-64 throughput, the runtime SIMD path against the forced-scalar one, from 256 B to 16 MiB.
//! `cargo xtask test --tier bench -p marlin-hash`, or
//! `cargo bench -p marlin-hash --bench simd -- [filter]`.

use marlin_bench::{Bench, black_box};
use marlin_hash::stable::scalar;
use marlin_hash::xxh3_64_with_seed;
use marlin_math::Rng;

/// Deterministic bytes, as in the unit tests' `simd.rs`.
fn data(n: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    (0..n).map(|_| (rng.next_u64() >> 24) as u8).collect()
}

fn throughput() {
    for size in [256, 1 << 10, 4 << 10, 64 << 10, 1 << 20, 16 << 20] {
        let d = data(size, 3);
        assert_eq!(
            xxh3_64_with_seed(&d, 0),
            scalar::xxh3_64_with_seed(&d, 0),
            "{size}"
        );
        Bench::new(format!("xxh3_64/{size}/scalar"))
            .throughput_bytes(size as u64)
            .run(|| scalar::xxh3_64_with_seed(black_box(&d), 0));
        Bench::new(format!("xxh3_64/{size}/simd"))
            .throughput_bytes(size as u64)
            .run(|| xxh3_64_with_seed(black_box(&d), 0));
    }
}

marlin_bench::main!(throughput);
