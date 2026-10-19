//! marlin-render's GPU tests (gpu tier, docs/design/0014-testing.md §5): the GPU Scene, resident
//! geometry, the visibility passes and the post chain on the hardware adapter with validation on,
//! read back, all on the shared device from `marlin_gpu_test_support`. Never create a `Device`
//! here.
//! Run alone with `cargo test -p marlin-render --test gpu`.
#![cfg(marlin_platform = "windows")]

#[path = "../support/mod.rs"]
mod support;

mod geometry;
mod gpu_scene;
mod occlusion;
mod post;
mod streaming;
mod visibility;
