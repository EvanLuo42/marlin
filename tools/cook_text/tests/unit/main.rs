//! marlin-cook-text's tests (quick tier, docs/design/0014-testing.md §5): string tables cooked
//! from `.masset` files (samples in tests/data), loaded in place and formatted.
//! Run alone with `cargo test -p marlin-cook-text --test unit`.

#[path = "../support/mod.rs"]
mod support;

mod cooker;
