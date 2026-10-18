//! marlin-ui-render's GPU tests (gpu tier, docs/design/0014-testing.md §5; 0012 §14): the tile
//! lists and the shading against the CPU reference, on the shared hardware device from
//! `marlin_gpu_test_support`, with validation on. Never create a `Device` here.
//! Run alone with `cargo test -p marlin-ui-render --test gpu`.
#![cfg(marlin_platform = "windows")]

#[path = "../support/mod.rs"]
mod support;

mod binning;
mod shading;

use marlin_gpu_test_support::{hardware_device, shader};
use marlin_render_graph::FrameContext;
use marlin_ui_render::{UI_ENTRIES, UiRenderer, UiRendererDesc, UiShaders};

/// A renderer on the shared device; `desc` as given.
pub fn renderer(desc: &UiRendererDesc) -> UiRenderer {
    let s = |i: usize| shader(UI_ENTRIES[i]);
    UiRenderer::new(
        hardware_device(),
        &UiShaders {
            glyph_upload: s(0),
            bounds: s(1),
            bin: s(2),
            coarse: s(3),
            fine: s(4),
            encode_vs: s(5),
            encode_ps: s(6),
        },
        desc,
    )
    .unwrap()
}

/// Waits for the GPU and drops `renderer`'s resources from `frames`.
pub fn finish(frames: &mut FrameContext, renderer: &UiRenderer) {
    frames.wait_idle().unwrap();
    renderer.forget(frames);
    assert_eq!(hardware_device().validation_errors(), 0);
}
