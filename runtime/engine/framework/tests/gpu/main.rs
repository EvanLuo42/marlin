//! marlin-framework's GPU tests (gpu tier, docs/design/0014-testing.md §5): the whole pipeline,
//! from cooking the sample to reading back rendered frames, on the shared hardware device from
//! `marlin_gpu_test_support`, with a cook service on a background thread where a test needs one.
//! Never create a `Device` here.
//! Run alone with `cargo test -p marlin-framework --test gpu`; `MARLIN_BLESS=1` rewrites the
//! golden images in `tests/data/golden/<backend>/` that no longer match.
#![cfg(marlin_platform = "windows")]

mod end_to_end;
