//! marlin-front-hir-def's tests (quick tier, docs/design/0014-testing.md §5): item trees,
//! package def maps and their diagnostics (golden files), C# name lookup and accessibility,
//! and incremental reuse across edits. Platform-independent.
//! Run alone with `cargo test -p marlin-front-hir-def --test unit`; `MARLIN_BLESS=1` rewrites
//! the golden files.

mod fixture;

mod def_map;
mod incremental;
mod resolve;
