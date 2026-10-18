//! marlin-render-graph's platform-independent tests (quick tier, docs/design/0014-testing.md §5):
//! graphs over the recording backend, checked through their compiled plans (0007 §12).
//! Run alone with `cargo test -p marlin-render-graph --test unit`.

#[path = "../support/mod.rs"]
mod support;

mod alias;
mod barriers;
mod cache;
mod cull;
// Development checks are compiled out without debug assertions.
#[cfg(debug_assertions)]
mod validation;
