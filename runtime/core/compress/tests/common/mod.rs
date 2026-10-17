//! Only for the flat `tests/bench.rs`, which still says `mod common;` until the benchmarks move
//! to the marlin-bench harness (docs/design/0014-testing.md §7.2); delete with it. The helpers are
//! in `tests/support/`.
#![allow(unused_imports)]

#[path = "../support/mod.rs"]
mod support;

pub use support::*;
