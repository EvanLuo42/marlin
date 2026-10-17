//! marlin-compress's tests (quick tier, docs/design/0014-testing.md §5): round trips, decoding
//! the reference implementations' frames in `tests/data/`, and damaged input. Platform-independent.
//! Run alone with `cargo test -p marlin-compress --test unit`.

#[path = "../support/mod.rs"]
mod support;

mod corrupt;
mod interop;
mod roundtrip;
