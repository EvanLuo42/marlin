//! marlin-cook-level's tests (quick tier, docs/design/0014-testing.md §5): levels and Prefabs
//! cooked through the pipeline and read back. Run alone with
//! `cargo test -p marlin-cook-level --test unit`.

#[path = "../support/mod.rs"]
mod support;

mod cooker;
