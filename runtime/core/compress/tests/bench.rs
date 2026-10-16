//! Throughput, for tracking. Run optimized:
//! `cargo test -p marlin-compress --release --test bench -- --ignored --nocapture`

mod common;

use marlin_compress::{Codec, Level, compress, decompress};
use std::time::{Duration, Instant};

/// Repeats `f` until at least `min` has passed; returns the mean time per call.
fn time(min: Duration, mut f: impl FnMut()) -> Duration {
    let start = Instant::now();
    let mut n = 0;
    while n == 0 || start.elapsed() < min {
        f();
        n += 1;
    }
    start.elapsed() / n
}

fn mb_s(bytes: usize, t: Duration) -> f64 {
    bytes as f64 / t.as_secs_f64() / 1e6
}

#[test]
#[ignore]
fn throughput() {
    // 1 MiB chunks, the largest package chunk size.
    let inputs = [
        ("text", common::text(1 << 20, 1)),
        ("binary", common::binary(1 << 20, 2)),
        ("mixed", {
            let mut v = common::text(512 << 10, 3);
            v.extend_from_slice(&common::binary(384 << 10, 4));
            v.extend_from_slice(&common::random(128 << 10, 5));
            v
        }),
    ];
    let min = Duration::from_millis(400);
    println!(
        "{:<9} {:<7} {:>5} {:>7} {:>12} {:>12}",
        "codec", "data", "level", "ratio", "comp MB/s", "decomp MB/s"
    );
    for codec in [Codec::Lz4, Codec::Zstd, Codec::GDeflate] {
        for (name, data) in &inputs {
            for level in [1, 5, 9] {
                let level = Level::new(level);
                let mut packed = Vec::new();
                let tc = time(min, || {
                    packed.clear();
                    compress(codec, level, data, &mut packed).unwrap();
                });
                let mut out = vec![0; data.len()];
                let td = time(min, || {
                    decompress(codec, &packed, &mut out).unwrap();
                });
                assert!(out == *data);
                println!(
                    "{:<9} {:<7} {:>5} {:>7.3} {:>12.1} {:>12.1}",
                    codec.name(),
                    name,
                    level.get(),
                    data.len() as f64 / packed.len() as f64,
                    mb_s(data.len(), tc),
                    mb_s(data.len(), td),
                );
            }
        }
    }
}
