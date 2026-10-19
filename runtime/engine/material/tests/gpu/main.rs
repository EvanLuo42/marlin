//! marlin-material's GPU tests (gpu tier, docs/design/0014-testing.md §5): the material passes
//! after the visibility buffer on the hardware adapter with validation on, read back, and the
//! material buffer's debug views against golden references (`tests/data/golden/<api>/`). All on
//! the shared device from `marlin_gpu_test_support`. Never create a `Device` here.
//! Run alone with `cargo test -p marlin-material --test gpu`.
#![cfg(marlin_platform = "windows")]

mod denoise;
mod gi;
mod materials;
mod restir;
mod scenes;
mod sharc;
mod simple;
mod support;
mod translucent;
mod tsr;
