//! marlin-render's golden images (gpu tier, docs/design/0014-testing.md §7.1): small scenes
//! rendered offscreen on the shared hardware device and compared with the references in
//! `tests/data/golden/<api>/`, `<api>` being `marlin_rhi::GRAPHICS_API.name()` (`vulkan`,
//! `d3d12`). Never create a `Device` here.
//! Run alone with `cargo test -p marlin-render --test golden`; `MARLIN_BLESS=1` rewrites the
//! references that no longer match.
#![cfg(marlin_platform = "windows")]

#[path = "../support/mod.rs"]
mod support;

mod scenes;
