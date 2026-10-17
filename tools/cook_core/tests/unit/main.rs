//! marlin-cook-core's tests (quick tier, docs/design/0014-testing.md §5): cooking throwaway
//! projects, the local derived data cache, and packages loaded back through the IO queue.
//! Run alone with `cargo test -p marlin-cook-core --test unit`.

#[path = "../support/mod.rs"]
mod support;

mod cook;
mod ddc;
mod package;
mod platform;
