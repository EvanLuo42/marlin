//! marlin-render's GPU tests (gpu tier, docs/design/0014-testing.md §5): the forward renderer on
//! the hardware adapter with validation on, scenes rendered offscreen and read back, all on the
//! shared device from `marlin_gpu_test_support`. Never create a `Device` here.
//! Run alone with `cargo test -p marlin-render --test gpu`.
#![cfg(marlin_platform = "windows")]

#[path = "../support/mod.rs"]
mod support;

mod forward;
mod geometry;
mod gpu_scene;
mod post;
mod visibility;
