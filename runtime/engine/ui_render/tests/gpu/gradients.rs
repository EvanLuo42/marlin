//! Gradients on the GPU: pixels against the reference and a golden image.

use marlin_gpu_test_support::{assert_image, hardware_device};
use marlin_math::Vec2;
use marlin_render_graph::{FrameContext, FrameContextDesc};
use marlin_ui_render::{
    Affine2, Color, Corners, FillRule, Gradient, GroupDesc, Paint, Path, Rect, UiDrawList,
    UiRendererDesc,
};

use crate::binning::compare;
use crate::shading::{close, draw};
use crate::support::{Pages, inter};
use crate::{finish, renderer};

#[test]
fn gradients_shade_like_the_reference() {
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let mut pages = Pages::new(r.glyph_cache().config().page_size);
    let mut list = UiDrawList::new(1.0);
    {
        let mut p = list.painter(&r.glyph_cache().ids);
        let font = inter();
        let rgb = [
            (0.0, Color::rgb(255, 40, 40)),
            (0.5, Color::rgb(40, 220, 60)),
            (1.0, Color::rgb(40, 80, 255)),
        ];
        p.fill_rect(
            Rect::new(0.0, 0.0, 320.0, 200.0),
            Corners::ZERO,
            &Paint::Solid(Color::rgb(16, 16, 20)),
        );
        // The same stops in both spaces.
        let srgb = Gradient::linear(Vec2::new(10.0, 0.0), Vec2::new(150.0, 0.0), &rgb);
        p.fill_rect(
            Rect::new(10.0, 10.0, 150.0, 30.0),
            Corners::all(4.0),
            &Paint::Gradient(&srgb),
        );
        let ok = srgb.clone().in_oklab();
        p.fill_rect(
            Rect::new(10.0, 36.0, 150.0, 56.0),
            Corners::all(4.0),
            &Paint::Gradient(&ok),
        );
        // A soft radial spot fading to transparent.
        let spot = Gradient::radial(
            Vec2::new(230.0, 50.0),
            45.0,
            &[
                (0.0, Color::rgba(255, 220, 120, 255)),
                (1.0, Color::rgba(255, 120, 40, 0)),
            ],
        );
        p.fill_rect(
            Rect::new(180.0, 0.0, 280.0, 100.0),
            Corners::ZERO,
            &Paint::Gradient(&spot),
        );
        // A conic ring as a path with a hole.
        let wheel = Gradient::angle(
            Vec2::new(70.0, 135.0),
            0.0,
            &[
                (0.0, Color::rgb(255, 0, 0)),
                (0.33, Color::rgb(0, 255, 0)),
                (0.66, Color::rgb(0, 0, 255)),
                (1.0, Color::rgb(255, 0, 0)),
            ],
        );
        let mut ring = Path::new();
        for (radius, dir) in [(50.0f32, 1.0f32), (30.0, -1.0)] {
            for i in 0..=48 {
                let a = dir * i as f32 / 48.0 * core::f32::consts::TAU;
                let q = Vec2::new(70.0 + radius * a.cos(), 135.0 + radius * a.sin());
                if i == 0 {
                    ring.move_to(q);
                } else {
                    ring.line_to(q);
                }
            }
            ring.close();
        }
        p.fill_path(&ring, FillRule::NonZero, &Paint::Gradient(&wheel));
        // Gradient text and a turned card.
        let text = Gradient::linear(Vec2::new(150.0, 130.0), Vec2::new(300.0, 130.0), &rgb);
        let layout =
            marlin_font::TextLayout::new(&font, marlin_font::FontId(0), 22.0, "Gradient text");
        p.text_paint(&layout, Vec2::new(150.0, 115.0), &Paint::Gradient(&text));
        p.begin_group(&GroupDesc {
            transform: Affine2::translate(230.0, 170.0) * Affine2::rotate(-0.2),
            opacity: 0.9,
            ..GroupDesc::default()
        });
        let card = Gradient::linear(
            Vec2::new(0.0, -15.0),
            Vec2::new(0.0, 15.0),
            &[
                (0.0, Color::rgb(90, 90, 110)),
                (1.0, Color::rgb(40, 40, 50)),
            ],
        );
        p.fill_rect(
            Rect::new(-60.0, -15.0, 60.0, 15.0),
            Corners::all(8.0),
            &Paint::Gradient(&card),
        );
        p.end_group();
    }
    let (gpu, cpu) = draw(
        &mut r,
        &mut frames,
        &mut pages,
        &list,
        320,
        200,
        Color::BLACK,
    );
    close(&gpu, &cpu, 3, 0.01);
    compare(&mut r, &mut frames, &list, 320, 200);
    assert_image!("ui_gradients", &gpu);
    finish(&mut frames, &r);
}
