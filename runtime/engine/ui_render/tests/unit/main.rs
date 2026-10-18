//! marlin-ui-render's platform-independent tests (quick tier, docs/design/0014-testing.md §5;
//! 0012 §14): primitive coverage against supersampled shapes, the reference binning, recording
//! in chunks and replaying retained ones.
//! Run alone with `cargo test -p marlin-ui-render --test unit`.

#[path = "../support/mod.rs"]
mod support;

mod binning;
mod recording;
mod sdf;
