//! Groups and clips on the GPU (U2a): tile lists under exactly representable transforms
//! against the reference's, and pixels under any transform within tolerance.

use marlin_gpu_test_support::{assert_image, hardware_device};
use marlin_math::Vec2;
use marlin_render_graph::{FrameContext, FrameContextDesc};
use marlin_ui_render::{
    Affine2, Color, Corners, GroupDesc, LineCap, Painter, Rect, RectStyle, ShapeStyle, Stroke,
    StrokeStyle, UiDrawList, UiRendererDesc,
};

use crate::binning::compare;
use crate::shading::{close, draw};
use crate::support::{Pages, inter};
use crate::{finish, renderer};

/// Nested groups and clips over panels; `rotation` is the turn of the middle group.
fn scene(p: &mut Painter<'_>, rotation: Affine2) {
    let font = inter();
    p.rect(
        Rect::new(0.0, 0.0, 320.0, 200.0),
        &RectStyle::fill(Color::rgb(24, 26, 30)),
    );
    // A scrolled panel: clipped, its content moved up.
    p.push_clip(Rect::new(10.0, 10.0, 150.0, 190.0), Corners::all(8.0));
    p.begin_group(&GroupDesc {
        transform: Affine2::translate(10.0, -14.25),
        ..GroupDesc::default()
    });
    for i in 0..9 {
        let y = 20.0 + i as f32 * 22.0;
        p.rect(
            Rect::new(4.0, y, 136.0, y + 18.0),
            &RectStyle::rounded(Color::rgb(50 + i * 8, 54, 64), 4.0),
        );
        let layout =
            marlin_font::TextLayout::new(&font, marlin_font::FontId(0), 12.0, "Row of a list");
        p.text(&layout, Vec2::new(10.0, y + 2.0), Color::rgb(230, 230, 235));
    }
    p.end_group();
    p.pop_clip();
    // A turned, scaled card with its own clip and half opacity.
    p.begin_group(&GroupDesc {
        transform: Affine2::translate(230.0, 100.0) * rotation * Affine2::scale(2.0, 2.0),
        opacity: 0.5,
        ..GroupDesc::default()
    });
    p.rect(
        Rect::new(-30.0, -20.0, 30.0, 20.0),
        &RectStyle {
            fill: Color::rgb(240, 160, 60),
            corners: Corners::all(5.0),
            stroke: Some(Stroke {
                width: 1.5,
                color: Color::WHITE,
            }),
        },
    );
    p.push_clip(Rect::new(-26.0, -16.0, 0.0, 16.0), Corners::ZERO);
    p.circle(
        Vec2::new(-4.0, 0.0),
        14.0,
        &ShapeStyle::fill(Color::rgb(40, 120, 230)),
    );
    p.pop_clip();
    p.line(
        Vec2::new(-28.0, 18.0),
        Vec2::new(28.0, -18.0),
        &StrokeStyle {
            width: 1.0,
            color: Color::WHITE,
            cap: LineCap::Round,
            dash: None,
        },
    );
    p.end_group();
}

/// A quarter turn with exact products, so the GPU's bounds equal the reference's.
const QUARTER: Affine2 = Affine2([0.0, 1.0, -1.0, 0.0, 0.0, 0.0]);

#[test]
fn groups_and_clips_bin_like_the_reference() {
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    for (w, h, scale) in [(320, 200, 1.0), (640, 400, 2.0)] {
        let mut list = UiDrawList::new(scale);
        scene(&mut list.painter(&r.glyph_cache().ids), QUARTER);
        compare(&mut r, &mut frames, &list, w, h);
    }
    finish(&mut frames, &r);
}

#[test]
fn groups_and_clips_shade_like_the_reference() {
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let mut pages = Pages::new(r.glyph_cache().config().page_size);
    let mut list = UiDrawList::new(1.0);
    scene(
        &mut list.painter(&r.glyph_cache().ids),
        Affine2::rotate(0.4),
    );
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
    assert_image!("ui_groups", &gpu);
    finish(&mut frames, &r);
}
