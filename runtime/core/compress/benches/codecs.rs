//! Codec throughput on 1 MiB chunks, the largest package chunk size.
//! `cargo xtask test --tier bench -p marlin-compress`, or
//! `cargo bench -p marlin-compress --bench codecs -- [filter]`.

#[path = "../tests/support/mod.rs"]
mod support;

use marlin_bench::Bench;
use marlin_compress::{Codec, Level, compress, decompress};

fn throughput() {
    let inputs = [
        ("text", support::text(1 << 20, 1)),
        ("binary", support::binary(1 << 20, 2)),
        ("mixed", {
            let mut v = support::text(512 << 10, 3);
            v.extend_from_slice(&support::binary(384 << 10, 4));
            v.extend_from_slice(&support::random(128 << 10, 5));
            v
        }),
    ];
    for codec in [Codec::Lz4, Codec::Zstd, Codec::GDeflate] {
        for (name, data) in &inputs {
            for level in [1, 5, 9] {
                let level = Level::new(level);
                let id = format!("{}/{name}/{}", codec.name(), level.get());
                let bytes = data.len() as u64;

                let mut packed = Vec::new();
                compress(codec, level, data, &mut packed).unwrap();
                let mut out = vec![0; data.len()];
                decompress(codec, &packed, &mut out).unwrap();
                assert!(out == *data, "{id}: round trip");

                let mut scratch = Vec::new();
                Bench::new(format!("{id}/compress"))
                    .throughput_bytes(bytes)
                    .metric("ratio", data.len() as f64 / packed.len() as f64)
                    .run(|| {
                        scratch.clear();
                        compress(codec, level, data, &mut scratch).unwrap();
                    });
                Bench::new(format!("{id}/decompress"))
                    .throughput_bytes(bytes)
                    .run(|| decompress(codec, &packed, &mut out).unwrap());
            }
        }
    }
}

marlin_bench::main!(throughput);
