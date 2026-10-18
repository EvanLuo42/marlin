//! marlin-render-graph's GPU tests (gpu tier, docs/design/0014-testing.md §5): frames run through
//! a `FrameContext` on the shared hardware device, read back and compared with what the passes
//! wrote (0007 §12). Run alone with `cargo test -p marlin-render-graph --test gpu`.
#![cfg(marlin_platform = "windows")]

mod frame;
mod queues;
mod retained;
