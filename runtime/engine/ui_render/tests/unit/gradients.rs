//! Gradients (§2.1, §8.2): the baked ramps in both spaces, premultiplied stops, each shape's
//! geometry, and gradients through groups and replays.

use marlin_font::GlyphIds;
use marlin_math::Vec2;
use marlin_ui_render::command::Chunk;
use marlin_ui_render::shape::{Scene, Textures, shade};
use marlin_ui_render::{
    Affine2, Color, Corners, FillRule, Gradient, GroupDesc, Paint, Painter, Path, Rect,
    RetainedChunk, UiDrawList,
};

struct NoTextures;

impl Textures for NoTextures {
    fn glyph(&self, _: u32, _: u32, _: u32) -> f32 {
        0.0
    }
    fn sample(&self, _: u32, _: u32, _: [f32; 2], _: f32) -> [f32; 4] {
        [0.0; 4]
    }
}

fn record(scale: f32, f: impl FnOnce(&mut Painter<'_>)) -> Chunk {
    let ids = GlyphIds::new();
    let mut list = UiDrawList::new(scale);
    f(&mut list.painter(&ids));
    list.flatten()
}

fn at(chunk: &Chunk, x: f32, y: f32) -> [f32; 4] {
    shade(
        &chunk.commands[0],
        &Scene::new(chunk, &[]),
        &NoTextures,
        x,
        y,
    )
}

fn close(a: [f32; 4], b: [f32; 4], tolerance: f32) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= tolerance)
}

const RED: Color = Color::rgb(255, 0, 0);
const BLUE: Color = Color::rgb(0, 0, 255);

#[test]
fn srgb_ramps_mix_encoded_values_and_oklab_ones_stay_bright() {
    let g = Gradient::linear(
        Vec2::ZERO,
        Vec2::new(100.0, 0.0),
        &[(0.0, RED), (1.0, BLUE)],
    );
    let mid = g.color_at(0.5);
    assert!(close(mid, [0.5, 0.0, 0.5, 1.0], 1e-6), "{mid:?}");
    let ok = g.clone().in_oklab().color_at(0.5);
    // Oklab's middle of red and blue is a lighter, bluer purple than sRGB's.
    assert!(ok[0] > 0.5 && ok[2] > 0.5, "{ok:?}");
    // The ends are the stops, whatever the space.
    let ok_g = g.in_oklab();
    assert!(close(ok_g.color_at(0.0), [1.0, 0.0, 0.0, 1.0], 1e-3));
    assert!(close(ok_g.color_at(1.0), [0.0, 0.0, 1.0, 1.0], 1e-3));
}

#[test]
fn stops_interpolate_premultiplied() {
    // From transparent black to opaque white: no dark band (premultiplied, the color is white
    // all the way, only alpha changes).
    let g = Gradient::linear(
        Vec2::ZERO,
        Vec2::new(1.0, 0.0),
        &[(0.0, Color::rgba(0, 0, 0, 0)), (1.0, Color::WHITE)],
    );
    let c = g.color_at(0.25);
    assert!(close(c, [0.25, 0.25, 0.25, 0.25], 1e-6), "{c:?}");
    // Beyond the stops, the end colors go on; uneven stops are honored.
    let g = Gradient::linear(Vec2::ZERO, Vec2::new(1.0, 0.0), &[(0.2, RED), (0.4, BLUE)]);
    assert!(close(g.color_at(0.1), [1.0, 0.0, 0.0, 1.0], 1e-6));
    assert!(close(g.color_at(0.3), [0.5, 0.0, 0.5, 1.0], 1e-6));
    assert!(close(g.color_at(0.9), [0.0, 0.0, 1.0, 1.0], 1e-6));
}

#[test]
fn each_shape_maps_pixels_to_its_ramp() {
    let stops = [(0.0, Color::BLACK), (1.0, Color::WHITE)];
    let fill = |g: &Gradient| {
        record(1.0, |p| {
            p.fill_rect(
                Rect::new(0.0, 0.0, 128.0, 128.0),
                Corners::ZERO,
                &Paint::Gradient(g),
            )
        })
    };
    // 1/255 of the ramp's texel spacing plus 8-bit rounding.
    let tol = 1.5 / 255.0;
    let linear = fill(&Gradient::linear(
        Vec2::new(20.0, 0.0),
        Vec2::new(120.0, 0.0),
        &stops,
    ));
    for (x, t) in [(10.0, 0.0), (45.0, 0.25), (70.0, 0.5), (120.0, 1.0)] {
        let c = at(&linear, x, 64.0);
        assert!(close(c, [t, t, t, 1.0], tol), "linear at {x}: {c:?}");
    }
    let radial = fill(&Gradient::radial(Vec2::new(64.0, 64.0), 40.0, &stops));
    for (d, t) in [(0.0, 0.0), (10.0, 0.25), (20.0, 0.5), (60.0, 1.0)] {
        let c = at(&radial, 64.0, 64.0 + d);
        assert!(close(c, [t, t, t, 1.0], tol), "radial at {d}: {c:?}");
    }
    // Clockwise from the right (y down): a quarter turn is straight down.
    let angle = fill(&Gradient::angle(Vec2::new(64.0, 64.0), 0.0, &stops));
    for (x, y, t) in [
        (100.0, 64.0, 0.0),
        (64.0, 100.0, 0.25),
        (28.0, 64.1, 0.5),
        (64.0, 28.0, 0.75),
    ] {
        let c = at(&angle, x, y);
        assert!(close(c, [t, t, t, 1.0], tol), "angle at ({x}, {y}): {c:?}");
    }
}

#[test]
fn gradients_follow_groups_and_replays() {
    let g = Gradient::linear(
        Vec2::ZERO,
        Vec2::new(10.0, 0.0),
        &[(0.0, Color::BLACK), (1.0, Color::WHITE)],
    );
    // In a group scaled 4× and moved, at a surface scale of 2: the ramp spans 80 physical
    // pixels from x = 20.
    let grouped = record(2.0, |p| {
        p.begin_group(&GroupDesc {
            transform: Affine2::translate(10.0, 0.0) * Affine2::scale(4.0, 4.0),
            opacity: 0.5,
            ..GroupDesc::default()
        });
        p.fill_rect(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            Corners::ZERO,
            &Paint::Gradient(&g),
        );
        p.end_group();
    });
    let c = at(&grouped, 60.0, 40.0);
    assert!(close(c, [0.25, 0.25, 0.25, 0.5], 2.0 / 255.0), "{c:?}");
    // A path in the same group: on the screen, it takes the group's opacity in the gradient.
    let path = record(2.0, |p| {
        p.begin_group(&GroupDesc {
            transform: Affine2::translate(10.0, 0.0) * Affine2::scale(4.0, 4.0),
            opacity: 0.5,
            ..GroupDesc::default()
        });
        let mut r = Path::new();
        r.move_to(Vec2::ZERO)
            .line_to(Vec2::new(10.0, 0.0))
            .line_to(Vec2::new(10.0, 10.0))
            .line_to(Vec2::new(0.0, 10.0));
        p.fill_path(&r, FillRule::NonZero, &Paint::Gradient(&g));
        p.end_group();
    });
    let c = at(&path, 60.0, 40.0);
    assert!(close(c, [0.25, 0.25, 0.25, 0.5], 2.0 / 255.0), "{c:?}");

    // A replay moves the gradient with the rectangle.
    let ids = GlyphIds::new();
    let retained = RetainedChunk::record(1.0, &ids, |p| {
        p.fill_rect(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            Corners::ZERO,
            &Paint::Gradient(&g),
        );
    });
    let mut list = UiDrawList::new(1.0);
    list.painter(&ids).replay(&retained, Vec2::new(30.0, 5.0));
    let moved = list.flatten();
    let c = at(&moved, 35.0, 10.0);
    assert!(close(c, [0.5, 0.5, 0.5, 1.0], 2.0 / 255.0), "{c:?}");
}
