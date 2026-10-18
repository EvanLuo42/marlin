//! marlin-rhi's GPU tests (gpu tier, docs/design/0014-testing.md §5): real work on the hardware
//! adapter with validation on, through each barrier path the backend has, on the shared devices
//! from `marlin_gpu_test_support::devices` (one per path). Never create a `Device` here: tests
//! that need their own device or process are the `isolated_gpu_*` binaries.
//! Run alone with `cargo test -p marlin-rhi --test gpu`.
#![cfg(marlin_platform = "windows")]

mod clears;
mod draw;
mod indirect;
mod memory;
mod mesh;
mod queries;
mod queues;
mod raster;
mod raytracing;
mod resources;
mod services;
mod shader_output;
mod sparse;
mod textures;
mod views;
