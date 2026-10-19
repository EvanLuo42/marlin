//! marlin-ui-render's GPU tests (gpu tier, docs/design/0014-testing.md §5; 0012 §14): the tile
//! lists and the shading against the CPU reference, on the shared hardware device from
//! `marlin_gpu_test_support`, with validation on. Never create a `Device` here.
//! Run alone with `cargo test -p marlin-ui-render --test gpu`.
#![cfg(marlin_platform = "windows")]

#[path = "../support/mod.rs"]
mod support;

mod binning;
mod blur;
mod damage;
mod gradients;
mod groups;
mod paths;
mod shading;
mod texture;

use marlin_font::{FontView, GlyphCache, GlyphRecord, GlyphUpload};
use marlin_gpu_test_support::{hardware_device, shader};
use marlin_render_graph::{FrameContext, RenderGraph};
use marlin_ui_render::command::Chunk;
use marlin_ui_render::{
    Color, UI_ENTRIES, UiDrawList, UiOutput, UiPrepared, UiRenderer, UiRendererDesc, UiShaders,
    UiSurface, UiSurfaceDesc,
};

/// A renderer and one surface: what most tests draw with.
pub struct TestUi {
    pub renderer: UiRenderer,
    pub surface: UiSurface,
}

/// One frame's CPU work for the test surface.
pub struct Frame(pub UiPrepared);

impl Frame {
    pub fn commands(&self) -> &Chunk {
        self.0.surfaces[0].commands()
    }
    pub fn glyph_table(&self) -> &[GlyphRecord] {
        self.0.glyph_table()
    }
    pub fn uploads(&self) -> &[GlyphUpload] {
        self.0.uploads()
    }
}

impl TestUi {
    pub fn glyph_cache(&self) -> &GlyphCache {
        self.renderer.glyph_cache()
    }

    pub fn prepare(&mut self, list: &UiDrawList, fonts: &[FontView<'_>], _: Option<()>) -> Frame {
        Frame(self.renderer.prepare(&[list], &[], fonts, None))
    }

    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        frame: &'g Frame,
        width: u32,
        height: u32,
        clear: Color,
    ) -> UiOutput<'g> {
        let pages = self.renderer.begin(g, &frame.0);
        let data = &frame.0.surfaces[0];
        self.renderer
            .add_passes(g, pages, &self.surface, data, width, height, clear, &[])
    }

    pub fn pool_overflowed(&self) -> bool {
        self.surface.pool_overflowed()
    }

    pub fn pool_blocks(&self) -> u32 {
        self.surface.pool_blocks()
    }

    pub fn forget(&self, frames: &mut FrameContext) {
        self.renderer.forget(frames);
        self.surface.forget(frames);
    }
}

/// A renderer on the shared device; `desc` as given.
pub fn renderer(desc: &UiRendererDesc) -> TestUi {
    renderer_with(desc, &UiSurfaceDesc::new(1))
}

pub fn renderer_with(desc: &UiRendererDesc, surface: &UiSurfaceDesc) -> TestUi {
    TestUi {
        renderer: shared_renderer(desc),
        surface: UiSurface::new(hardware_device(), surface).unwrap(),
    }
}

pub fn shared_renderer(desc: &UiRendererDesc) -> UiRenderer {
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
            blur: s(7),
            path_tiles: s(8),
            coarse_merge: s(9),
            coarse_copy: s(10),
            coarse_plan: s(11),
        },
        desc,
    )
    .unwrap()
}

/// Waits for the GPU and drops `renderer`'s resources from `frames`.
pub fn finish(frames: &mut FrameContext, renderer: &TestUi) {
    frames.wait_idle().unwrap();
    renderer.forget(frames);
    assert_eq!(hardware_device().validation_errors(), 0);
}

/// The compiled entries take the push constants the renderer sets (what the engine checks when
/// it loads the cooked library).
#[test]
fn entries_take_the_push_constants_the_renderer_sets() {
    for (name, push) in UI_ENTRIES.iter().zip(marlin_ui_render::UI_PUSH_BYTES) {
        let reflection = marlin_gpu_test_support::Shader::new(name).reflection();
        assert_eq!(reflection.push.as_ref().map(|p| p.size), push, "{name}");
    }
}
