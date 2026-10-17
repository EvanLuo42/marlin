//! marlin-hash's tests (quick tier, docs/design/0014-testing.md §5): pinned vectors for the stable
//! hashes, streaming against one-shot, SIMD against scalar, and the hashers' behavior.
//! Platform-independent. Run alone with `cargo test -p marlin-hash --test unit`; the ignored
//! throughput benchmark is in `simd`.

mod crc32;
mod fast;
mod guid;
mod keyed;
mod simd;
mod stable;
mod stable_hash;
mod stream;
