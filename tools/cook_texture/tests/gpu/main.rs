//! marlin-cook-texture's GPU tests (gpu tier, docs/design/0014-testing.md §5): encoded textures
//! decoded by the hardware adapter with validation on, on the shared device from
//! `marlin_gpu_test_support`. Never create a `Device` here. The readback shader is a placeholder
//! DXIL blob in tests/data/shaders (see `conformance`).
//! Run alone with `cargo test -p marlin-cook-texture --test gpu`.
#![cfg(marlin_platform = "windows")]

#[path = "../support/mod.rs"]
mod support;

mod conformance;
