//! Backdrop blurs on the GPU (U3b, §5.6): the layers, the blurred images and their compositing
//! against the reference, tile lists included; a golden image; and a frame whose change under a
//! blur, outside the blur's own tiles, still shows through it.

use marlin_font::{FontId, TextLayout};
use marlin_gpu_test_support::{assert_image, hardware_device};
use marlin_math::Vec2;
use marlin_render_graph::{FrameContext, FrameContextDesc};
use marlin_ui_render::{
    Affine2, Color, Corners, Gradient, GroupDesc, Paint, Painter, Rect, RectStyle, ShapeStyle,
    UiDrawList, UiRendererDesc,
};

use crate::binning::compare;
use crate::shading::{close, draw};
use crate::support::{Pages, inter};
use crate::{finish, renderer};

/// Stripes, circles and text behind two frosted panels side by side, a third over the first
/// one's text, and a turned, half-transparent one inside a clip. `dot` moves a circle.
fn scene(p: &mut Painter<'_>, dot: f32) {
    let font = inter();
    let g = Gradient::linear(
        Vec2::ZERO,
        Vec2::new(360.0, 220.0),
        &[
            (0.0, Color::rgb(250, 120, 60)),
            (1.0, Color::rgb(40, 80, 220)),
        ],
    );
    p.fill_rect(
        Rect::new(0.0, 0.0, 360.0, 220.0),
        Corners::ZERO,
        &Paint::Gradient(&g),
    );
    for i in 0..12 {
        let x = i as f32 * 30.0;
        p.rect(
            Rect::new(x, 0.0, x + 12.0, 220.0),
            &RectStyle::fill(Color::rgba(255, 255, 255, 90)),
        );
    }
    p.circle(
        Vec2::new(dot, 70.0),
        22.0,
        &ShapeStyle::fill(Color::rgb(30, 200, 120)),
    );
    let title = TextLayout::new(&font, FontId(0), 28.0, "Backdrop");
    p.text(&title, Vec2::new(14.0, 8.0), Color::rgb(20, 20, 30));

    // Two panels apart: one layer.
    p.backdrop_blur(
        Rect::new(20.0, 40.0, 150.0, 120.0),
        12.0,
        Corners::all(12.0),
    );
    p.rect(
        Rect::new(20.0, 40.0, 150.0, 120.0),
        &RectStyle {
            fill: Color::rgba(255, 255, 255, 60),
            corners: Corners::all(12.0),
            stroke: None,
        },
    );
    let label = TextLayout::new(&font, FontId(0), 14.0, "frosted");
    p.text(&label, Vec2::new(32.0, 52.0), Color::rgb(10, 10, 20));
    p.backdrop_blur(
        Rect::new(200.0, 30.0, 340.0, 100.0),
        30.0,
        Corners::all(6.0),
    );
    // Over the first panel's label: the next layer.
    p.backdrop_blur(Rect::new(60.0, 60.0, 130.0, 160.0), 6.0, Corners::all(20.0));
    // Turned and half transparent, clipped.
    p.push_clip(Rect::new(180.0, 110.0, 350.0, 210.0), Corners::all(8.0));
    p.begin_group(&GroupDesc {
        transform: Affine2::translate(260.0, 160.0) * Affine2::rotate(0.35),
        opacity: 0.8,
        ..GroupDesc::default()
    });
    p.backdrop_blur(
        Rect::new(-70.0, -35.0, 70.0, 35.0),
        10.0,
        Corners::all(10.0),
    );
    p.end_group();
    p.pop_clip();
}

#[test]
fn backdrop_blurs_draw_like_the_reference() {
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let mut pages = Pages::new(r.glyph_cache().config().page_size);
    let (w, h) = (360, 220);
    for (frame, dot) in [(0, 60.0), (1, 66.0)] {
        let mut list = UiDrawList::new(1.0);
        scene(&mut list.painter(&r.glyph_cache().ids), dot);
        // The second frame moves the circle under the panels: every tile that shows it changes,
        // also where the circle's own tiles stay the same.
        let (gpu, cpu) = draw(&mut r, &mut frames, &mut pages, &list, w, h, Color::BLACK);
        // Bilinear weights and exp() differ by a level here and there, and the blurred images
        // round them twice more.
        close(&gpu, &cpu, 4, 0.03);
        compare(&mut r, &mut frames, &list, w, h);
        if frame == 0 {
            assert_image!("ui_backdrop_blur", &gpu);
        }
    }
    finish(&mut frames, &r);
}
