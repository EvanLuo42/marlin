//! Path fill on the GPU (U2c): tile lists and pixels against the reference, and a golden image.

use marlin_gpu_test_support::{assert_image, hardware_device};
use marlin_math::Vec2;
use marlin_render_graph::{FrameContext, FrameContextDesc};
use marlin_ui_render::{
    Affine2, Color, Corners, FillRule, GroupDesc, Paint, Painter, Path, Rect, RectStyle,
    UiDrawList, UiRendererDesc,
};

use crate::binning::compare;
use crate::shading::{close, draw};
use crate::support::Pages;
use crate::{finish, renderer};

fn star(c: Vec2, r: f32) -> Path {
    let mut p = Path::new();
    for i in 0..5 {
        let a = -core::f32::consts::FRAC_PI_2 + i as f32 * 4.0 * core::f32::consts::PI / 5.0;
        let q = c + Vec2::new(a.cos(), a.sin()) * r;
        if i == 0 {
            p.move_to(q);
        } else {
            p.line_to(q);
        }
    }
    p.close();
    p
}

/// A heart of cubics, a ring of quadratics with a hole, stars by both rules, a tall sliver and
/// a turned, clipped icon.
fn scene(p: &mut Painter<'_>) {
    p.rect(
        Rect::new(0.0, 0.0, 320.0, 200.0),
        &RectStyle::fill(Color::rgb(20, 22, 28)),
    );
    let mut heart = Path::new();
    heart
        .move_to(Vec2::new(60.0, 60.0))
        .cubic_to(
            Vec2::new(60.0, 40.0),
            Vec2::new(25.0, 35.0),
            Vec2::new(25.0, 62.0),
        )
        .cubic_to(
            Vec2::new(25.0, 85.0),
            Vec2::new(55.0, 95.0),
            Vec2::new(60.0, 110.0),
        )
        .cubic_to(
            Vec2::new(65.0, 95.0),
            Vec2::new(95.0, 85.0),
            Vec2::new(95.0, 62.0),
        )
        .cubic_to(
            Vec2::new(95.0, 35.0),
            Vec2::new(60.0, 40.0),
            Vec2::new(60.0, 60.0),
        )
        .close();
    p.fill_path(
        &heart,
        FillRule::NonZero,
        &Paint::Solid(Color::rgb(230, 60, 80)),
    );
    let mut ring = Path::new();
    for (r, cw) in [(34.0f32, true), (20.0, false)] {
        let c = Vec2::new(160.0, 70.0);
        let pts: Vec<Vec2> = (0..8)
            .map(|i| {
                let a = i as f32 * core::f32::consts::FRAC_PI_4 * if cw { 1.0 } else { -1.0 };
                c + Vec2::new(a.cos(), a.sin()) * r
            })
            .collect();
        ring.move_to(pts[0]);
        for i in 0..8 {
            // Control points on the circle's tangents (a coarse quadratic circle).
            let (a, b) = (pts[i], pts[(i + 1) % 8]);
            let mid = (a + b) * 0.5;
            let ctrl = c + (mid - c) * (r / (mid - c).length() * 1.08);
            ring.quad_to(ctrl, b);
        }
        ring.close();
    }
    p.fill_path(
        &ring,
        FillRule::NonZero,
        &Paint::Solid(Color::rgb(80, 200, 140)),
    );
    p.fill_path(
        &star(Vec2::new(250.0, 60.0), 40.0),
        FillRule::NonZero,
        &Paint::Solid(Color::rgb(250, 210, 60)),
    );
    p.fill_path(
        &star(Vec2::new(250.0, 150.0), 40.0),
        FillRule::EvenOdd,
        &Paint::Solid(Color::rgb(250, 210, 60)),
    );
    let mut sliver = Path::new();
    sliver
        .move_to(Vec2::new(110.0, 2.0))
        .line_to(Vec2::new(113.0, 2.0))
        .line_to(Vec2::new(140.0, 198.0))
        .line_to(Vec2::new(137.0, 198.0))
        .close();
    p.fill_path(
        &sliver,
        FillRule::NonZero,
        &Paint::Solid(Color::rgba(255, 255, 255, 160)),
    );
    p.push_clip(Rect::new(20.0, 120.0, 100.0, 190.0), Corners::all(12.0));
    p.begin_group(&GroupDesc {
        transform: Affine2::translate(60.0, 155.0)
            * Affine2::rotate(0.5)
            * Affine2::scale(1.5, 1.5),
        opacity: 0.8,
        ..GroupDesc::default()
    });
    p.fill_path(
        &star(Vec2::ZERO, 30.0),
        FillRule::EvenOdd,
        &Paint::Solid(Color::rgb(90, 150, 255)),
    );
    p.end_group();
    p.pop_clip();
}

#[test]
fn paths_bin_and_shade_like_the_reference() {
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let mut pages = Pages::new(r.glyph_cache().config().page_size);
    let mut list = UiDrawList::new(1.0);
    scene(&mut list.painter(&r.glyph_cache().ids));
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
    assert_image!("ui_paths", &gpu);
    finish(&mut frames, &r);
}

/// Text from its outlines: a large title, turned text, and zooming text in a gradient.
#[test]
fn text_as_paths_shades_like_the_reference() {
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let mut pages = Pages::new(r.glyph_cache().config().page_size);
    let font = crate::support::inter();
    let fonts = [font];
    let mut list = UiDrawList::new(1.0);
    {
        let mut p = list.painter(&r.glyph_cache().ids).with_fonts(&fonts);
        p.rect(
            Rect::new(0.0, 0.0, 320.0, 200.0),
            &RectStyle::fill(Color::rgb(245, 245, 240)),
        );
        let title = marlin_font::TextLayout::new(&font, marlin_font::FontId(0), 96.0, "Ag");
        p.text(&title, Vec2::new(8.0, -10.0), Color::rgb(30, 30, 40));
        let turned =
            marlin_font::TextLayout::new(&font, marlin_font::FontId(0), 18.0, "turned text");
        p.begin_group(&GroupDesc {
            transform: Affine2::translate(170.0, 40.0) * Affine2::rotate(0.5),
            ..GroupDesc::default()
        });
        p.text(&turned, Vec2::ZERO, Color::rgb(200, 40, 60));
        p.end_group();
        p.set_text_as_paths(true);
        let zoom = marlin_font::TextLayout::new(&font, marlin_font::FontId(0), 24.0, "zooming");
        let g = marlin_ui_render::Gradient::linear(
            Vec2::new(150.0, 0.0),
            Vec2::new(300.0, 0.0),
            &[
                (0.0, Color::rgb(40, 90, 255)),
                (1.0, Color::rgb(255, 120, 40)),
            ],
        );
        p.text_paint(&zoom, Vec2::new(150.0, 140.0), &Paint::Gradient(&g));
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
    assert_image!("ui_text_paths", &gpu);
    finish(&mut frames, &r);
}
