//! marlin-editor's GPU tests (gpu tier, docs/design/0014-testing.md §5): the window shell drawn
//! by the UI renderer offscreen, against the CPU reference and its golden image, on the shared
//! hardware device. Never create a `Device` here.
//! Run alone with `cargo test -p marlin-editor --test gpu`.
#![cfg(marlin_platform = "windows")]

use marlin_editor::shell::{self, ShellState};
use marlin_font::FontView;
use marlin_gpu_test_support::{Image, assert_image, hardware_device, shader};
use marlin_math::Vec2;
use marlin_render_graph::{FrameContext, FrameContextDesc};
use marlin_ui_render::shape::Textures;
use marlin_ui_render::{
    Color, UI_ENTRIES, UiDrawList, UiRenderer, UiRendererDesc, UiShaders, UiSurface, UiSurfaceDesc,
    reference,
};

fn inter() -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../content/engine/fonts/Inter-Regular.ttf");
    let file = std::fs::read(path).expect("content/engine/fonts/Inter-Regular.ttf (Git LFS)");
    marlin_cook_font::cook_font(&file, 0).unwrap()
}

/// The glyph pages as uploaded; the shell has no images.
struct Pages(Vec<Vec<u8>>, u32);

impl Textures for Pages {
    fn glyph(&self, page: u32, x: u32, y: u32) -> f32 {
        self.0[page as usize][(y * self.1 + x) as usize] as f32 / 255.0
    }
    fn sample(&self, _: u32, _: u32, _: [f32; 2], _: f32) -> [f32; 4] {
        [0.0; 4]
    }
}

#[test]
fn the_shell_draws_like_the_reference_and_its_golden() {
    let payload = inter();
    let font = FontView::new(&payload).unwrap();
    let s = |i: usize| shader(UI_ENTRIES[i]);
    let mut ui = UiRenderer::new(
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
        &UiRendererDesc::default(),
    )
    .unwrap();
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    // 1.25 × DPI: a 1200 × 720 window of 960 × 576 logical pixels, the cursor over "Edit".
    let (w, h, scale) = (1200u32, 720u32, 1.25f32);
    let mut list = UiDrawList::new(scale);
    let fonts = [font];
    shell::draw(
        &mut list.painter(&ui.glyph_cache().ids).with_fonts(&fonts),
        font,
        &ShellState {
            size: Vec2::new(w as f32 / scale, h as f32 / scale),
            cursor: Some(Vec2::new(75.0, 14.0)),
            status: "Ready  ·  60 fps",
            viewport: None,
        },
    );
    let surface = UiSurface::new(hardware_device(), &UiSurfaceDesc::new(1)).unwrap();
    let prepared = ui.prepare(&[&list], &[], &[font], None);
    let data = &prepared.surfaces[0];
    let ticket = frames
        .execute(hardware_device(), |g, _| {
            let pages = ui.begin(g, &prepared);
            let out = ui.add_passes(g, pages, &surface, data, w, h, Color::BLACK, &[]);
            g.readback_texture(out.target)
        })
        .unwrap();
    let gpu = Image::new(w, h, frames.wait_readback(ticket).unwrap());

    let size = ui.glyph_cache().config().page_size;
    let mut pages = Pages(Vec::new(), size);
    for u in prepared.uploads() {
        while pages.0.len() <= u.page as usize {
            pages.0.push(vec![0; (size * size) as usize]);
        }
        for y in 0..u.rect.height {
            for x in 0..u.rect.width {
                pages.0[u.page as usize][((u.rect.y + y) * size + u.rect.x + x) as usize] =
                    u.coverage[(y * u.rect.width + x) as usize];
            }
        }
    }
    let clear = marlin_ui_render::shape::premultiplied(Color::BLACK.packed());
    let cpu = reference::rasterize(data.commands(), prepared.glyph_table(), &pages, w, h, clear);
    let worst = gpu
        .rgba8
        .iter()
        .zip(&cpu)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    assert!(
        worst <= 3,
        "the GPU is {worst} levels off the reference somewhere"
    );
    assert_image!("editor_shell", &gpu);

    frames.wait_idle().unwrap();
    ui.forget(&mut frames);
    surface.forget(&mut frames);
    assert_eq!(hardware_device().validation_errors(), 0);
}
