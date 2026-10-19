//! Texture surfaces (U3c, §10): the UI drawn into a texture with mips, read back against the
//! reference, and sampled through the surface's stable view by another surface.

use marlin_font::{FontId, FontView, TextLayout};
use marlin_gpu_test_support::{Image, hardware_device};
use marlin_math::Vec2;
use marlin_render_graph::{FrameContext, FrameContextDesc};
use marlin_ui_render::{
    Color, Corners, ExternalImageId, ImageColor, ImageSource, ImageStyle, Rect, RectStyle,
    ShapeStyle, UiDrawList, UiRendererDesc, UiSurface, UiSurfaceDesc, UiTextureDesc, reference,
};

use crate::shading::close;
use crate::support::{CpuTexture, CpuTextures, Pages, inter};
use crate::{TestUi, finish, shared_renderer};

const W: u32 = 120;
const H: u32 = 72;

fn panel(list: &mut UiDrawList, ids: &marlin_font::GlyphIds, x: f32) {
    let font = inter();
    let mut p = list.painter(ids);
    p.rect(
        Rect::new(0.0, 0.0, W as f32, H as f32),
        &RectStyle::fill(Color::rgb(24, 26, 32)),
    );
    p.rect(
        Rect::new(6.0, 6.0, 114.0, 66.0),
        &RectStyle {
            fill: Color::rgb(60, 110, 200),
            corners: Corners::all(9.0),
            stroke: None,
        },
    );
    p.circle(
        Vec2::new(x, 40.0),
        14.0,
        &ShapeStyle::fill(Color::rgb(240, 190, 60)),
    );
    let label = TextLayout::new(&font, FontId(0), 16.0, "in world");
    p.text(&label, Vec2::new(14.0, 10.0), Color::WHITE);
}

/// The 2 × 2 average of `src`, as the mip pass makes it (a last odd row or column dropped).
fn half(src: &Image) -> Image {
    let (w, h) = ((src.width / 2).max(1), (src.height / 2).max(1));
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            for k in 0..4 {
                let mut sum = 0.0;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (sx, sy) = (
                        (2 * x + dx).min(src.width - 1),
                        (2 * y + dy).min(src.height - 1),
                    );
                    sum += src.pixel(sx, sy)[k] as f32 / 255.0;
                }
                out.push(((sum / 4.0) * 255.0 + 0.5) as u8);
            }
        }
    }
    Image::new(w, h, out)
}

#[test]
fn a_texture_surface_draws_its_mips_and_samples_through_its_view() {
    let device = hardware_device();
    let mut r = TestUi {
        renderer: shared_renderer(&UiRendererDesc::default()),
        surface: UiSurface::new_texture(
            device,
            &UiSurfaceDesc::new(1),
            &UiTextureDesc {
                width: W,
                height: H,
                mips: true,
            },
        )
        .unwrap(),
    };
    let mut frames = FrameContext::new(device, &FrameContextDesc::default()).unwrap();
    let mut pages = Pages::new(r.glyph_cache().config().page_size);
    let view = r.surface.view().unwrap().index();
    assert_eq!(r.surface.size(), Some((W, H)));
    let fonts: [FontView<'_>; 1] = [inter()];

    for (frame, x) in [(0, 40.0), (1, 52.0)] {
        // A new window each frame: its list is the same both frames, and an image through a fixed
        // view does not tell damage detection that its texels changed (external images do).
        let window = UiSurface::new(device, &UiSurfaceDesc::new(2 + frame)).unwrap();
        let mut list = UiDrawList::new(1.0);
        panel(&mut list, &r.renderer.glyph_cache().ids, x);
        // The window shows the texture at 1:1 through the stable view.
        let mut shown = UiDrawList::new(1.0);
        shown.painter(&r.renderer.glyph_cache().ids).image(
            Rect::new(0.0, 0.0, W as f32, H as f32),
            ImageSource::Texture {
                view,
                width: W,
                height: H,
                color: ImageColor::LinearStraight,
            },
            &ImageStyle::default(),
        );
        let prepared = r.renderer.prepare(&[&list, &shown], &[], &fonts, None);
        let ticket = frames
            .execute(device, |g, _| {
                let pages = r.renderer.begin(g, &prepared);
                let out = r.renderer.add_passes(
                    g,
                    pages,
                    &r.surface,
                    &prepared.surfaces[0],
                    W,
                    H,
                    Color::BLACK,
                    &[],
                );
                // Passed as an external so the window's pass waits for the texture and reads it
                // in a sampled state; the image itself goes through the stable view.
                let win = r.renderer.add_passes(
                    g,
                    pages,
                    &window,
                    &prepared.surfaces[1],
                    W,
                    H,
                    Color::BLACK,
                    &[(ExternalImageId(9), out.target)],
                );
                [
                    g.readback_texture(out.target.mip(0)),
                    g.readback_texture(out.target.mip(1)),
                    g.readback_texture(out.target.mip(6)),
                    g.readback_texture(win.target),
                ]
            })
            .unwrap();
        let [mip0, mip1, last, win] = ticket.map(|t| frames.wait_readback(t).unwrap());
        let mip0 = Image::new(W, H, mip0);
        let mip1 = Image::new(W / 2, H / 2, mip1);
        assert_eq!(last.len(), 4, "the chain ends at 1 × 1");
        pages.apply(prepared.uploads());
        let texture = CpuTexture::checker();
        let cpu = reference::rasterize(
            prepared.surfaces[0].commands(),
            prepared.glyph_table(),
            &CpuTextures {
                pages: &pages,
                texture: &texture,
            },
            W,
            H,
            [0.0, 0.0, 0.0, 1.0],
        );
        close(&mip0, &Image::new(W, H, cpu), 3, 0.01);
        close(&mip1, &half(&mip0), 1, 0.0);
        // Opaque: decoded by the view, encoded again by the image, the same to a level.
        close(&Image::new(W, H, win), &mip0, 1, 0.0);
        assert_eq!(r.surface.view().unwrap().index(), view, "frame {frame}");
        frames.wait_idle().unwrap();
        window.forget(&mut frames);
    }
    finish(&mut frames, &r);
}
