//! marlin-cook-font's tests (quick tier, docs/design/0014-testing.md §5): TrueType files (a
//! synthetic one written by [`synth`], and the engine's Inter) cooked and read back.
//! Run alone with `cargo test -p marlin-cook-font --test unit`.

#[path = "../support/mod.rs"]
mod support;

mod cooker;
mod synth;
