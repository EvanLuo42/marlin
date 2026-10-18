//! Every primitive's coverage against its exact shape, supersampled.

use marlin_font::GlyphIds;
use marlin_math::Vec2;
use marlin_ui_render::command::Chunk;
use marlin_ui_render::shape::{Textures, segment_distance, shade, triangle_distance};
use marlin_ui_render::{
    Color, Corners, Dash, LineCap, Rect, RectStyle, ShapeStyle, StrokeStyle, UiDrawList,
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

fn one(f: impl FnOnce(&mut marlin_ui_render::Painter<'_>)) -> Chunk {
    let ids = GlyphIds::new();
    let mut list = UiDrawList::new(1.0);
    f(&mut list.painter(&ids));
    list.flatten()
}

/// The command's alpha against the fraction of 32 × 32 samples per pixel `inside` accepts, over
/// a 64 × 64 area: worst and mean difference.
fn compare(chunk: &Chunk, inside: impl Fn(f32, f32) -> bool) -> (f32, f32) {
    let n = 32;
    let (mut worst, mut sum) = (0.0f32, 0.0);
    for py in 0..64 {
        for px in 0..64 {
            let a = shade(
                &chunk.commands[0],
                &chunk.payload,
                &chunk.paints,
                &[],
                &NoTextures,
                px as f32 + 0.5,
                py as f32 + 0.5,
            )[3];
            let mut k = 0;
            for sy in 0..n {
                for sx in 0..n {
                    let x = px as f32 + (sx as f32 + 0.5) / n as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / n as f32;
                    k += u32::from(inside(x, y));
                }
            }
            let exact = k as f32 / (n * n) as f32;
            worst = worst.max((a - exact).abs());
            sum += (a - exact).abs();
        }
    }
    (worst, sum / (64.0 * 64.0))
}

/// The one-pixel ramp is exact for axis-aligned edges and close for others: its worst case is
/// at 45°, a corner of a pixel (about 0.09).
fn check(name: &str, (worst, mean): (f32, f32), max_worst: f32) {
    assert!(worst <= max_worst, "{name}: worst {worst}");
    assert!(mean < 0.01, "{name}: mean {mean}");
}

#[test]
fn rectangles() {
    let r = Rect::new(10.25, 7.5, 50.75, 40.0);
    let chunk = one(|p| p.rect(r, &RectStyle::fill(Color::WHITE)));
    let inside = |x: f32, y: f32| x >= r.min.x && x < r.max.x && y >= r.min.y && y < r.max.y;
    // Axis-aligned edges: exact but for the corners.
    check("rect", compare(&chunk, inside), 0.13);

    let radius = 9.0;
    let chunk = one(|p| p.rect(r, &RectStyle::rounded(Color::WHITE, radius)));
    let rounded = |x: f32, y: f32| {
        let cx = x.clamp(r.min.x + radius, r.max.x - radius);
        let cy = y.clamp(r.min.y + radius, r.max.y - radius);
        inside(x, y) && (x - cx).hypot(y - cy) <= radius
    };
    check("rounded rect", compare(&chunk, rounded), 0.1);
}

#[test]
fn per_corner_radii_and_clamping() {
    let r = Rect::new(4.0, 4.0, 60.0, 30.0);
    let corners = Corners {
        top_left: 0.0,
        top_right: 40.0, // clamped to half the height, 13
        bottom_right: 5.0,
        bottom_left: 9.5,
    };
    let chunk = one(|p| {
        p.rect(
            r,
            &RectStyle {
                fill: Color::WHITE,
                corners,
                stroke: None,
            },
        )
    });
    let radii = [0.0, 13.0, 5.0, 9.5];
    let shape = |x: f32, y: f32| {
        if !(x >= r.min.x && x < r.max.x && y >= r.min.y && y < r.max.y) {
            return false;
        }
        let right = x > (r.min.x + r.max.x) / 2.0;
        let bottom = y > (r.min.y + r.max.y) / 2.0;
        let rad = match (right, bottom) {
            (false, false) => radii[0],
            (true, false) => radii[1],
            (true, true) => radii[2],
            (false, true) => radii[3],
        };
        let cx = x.clamp(r.min.x + rad, r.max.x - rad);
        let cy = y.clamp(r.min.y + rad, r.max.y - rad);
        (x - cx).hypot(y - cy) <= rad
    };
    check("corners", compare(&chunk, shape), 0.1);
}

#[test]
fn strokes_straddle_the_edge() {
    let (c, radius, width) = (Vec2::new(32.0, 30.5), 20.0, 4.0);
    let chunk = one(|p| {
        p.circle(
            c,
            radius,
            &ShapeStyle {
                fill: Color::TRANSPARENT,
                stroke: Some(marlin_ui_render::Stroke {
                    width,
                    color: Color::WHITE,
                }),
            },
        )
    });
    let ring = |x: f32, y: f32| ((x - c.x).hypot(y - c.y) - radius).abs() <= width / 2.0;
    check("ring", compare(&chunk, ring), 0.1);
}

#[test]
fn circles() {
    let (c, radius) = (Vec2::new(30.3, 33.7), 17.6);
    let chunk = one(|p| p.circle(c, radius, &ShapeStyle::fill(Color::WHITE)));
    check(
        "circle",
        compare(&chunk, |x, y| (x - c.x).hypot(y - c.y) <= radius),
        0.1,
    );
}

#[test]
fn lines_with_every_cap() {
    let (a, b, width) = (Vec2::new(8.0, 12.0), Vec2::new(52.0, 47.0), 6.0);
    let len = (b - a).length();
    let dir = (b - a) / len;
    for cap in [LineCap::Butt, LineCap::Round, LineCap::Square] {
        let chunk = one(|p| {
            p.line(
                a,
                b,
                &StrokeStyle {
                    width,
                    color: Color::WHITE,
                    cap,
                    dash: None,
                },
            )
        });
        let inside = |x: f32, y: f32| {
            let r = Vec2::new(x, y) - a;
            let u = r.dot(dir);
            let v = r.x * dir.y - r.y * dir.x;
            match cap {
                LineCap::Butt => (0.0..=len).contains(&u) && v.abs() <= width / 2.0,
                LineCap::Square => {
                    (-width / 2.0..=len + width / 2.0).contains(&u) && v.abs() <= width / 2.0
                }
                LineCap::Round => segment_distance([x, y], [a.x, a.y], [b.x, b.y]) <= width / 2.0,
            }
        };
        // Rotated corners (the square and butt ends) are where the ramp is furthest off.
        check(&format!("{cap:?}"), compare(&chunk, inside), 0.2);
    }
}

#[test]
fn dashes_follow_the_arc_length() {
    let (a, b) = (Vec2::new(4.0, 30.0), Vec2::new(60.0, 30.0));
    let (dash, gap, phase) = (7.0, 4.0, 3.0);
    let chunk = one(|p| {
        p.line(
            a,
            b,
            &StrokeStyle {
                width: 5.0,
                color: Color::WHITE,
                cap: LineCap::Butt,
                dash: Some(Dash { dash, gap, phase }),
            },
        )
    });
    let inside = |x: f32, y: f32| {
        let u = x - a.x;
        let along = (u + phase).rem_euclid(dash + gap);
        (0.0..=b.x - a.x).contains(&u) && (y - 30.0).abs() <= 2.5 && along <= dash
    };
    check("dashes", compare(&chunk, inside), 0.13);
}

#[test]
fn triangles_either_winding() {
    let pts = [[6.0, 5.0], [58.0, 20.0], [21.0, 57.5]];
    for flip in [false, true] {
        let mut p3 = pts;
        if flip {
            p3.swap(1, 2);
        }
        let chunk = one(|p| {
            p.triangle(
                p3.map(|q| Vec2::new(q[0], q[1])),
                &ShapeStyle::fill(Color::WHITE),
            )
        });
        let inside = |x: f32, y: f32| triangle_distance([x, y], pts[0], pts[1], pts[2]) <= 0.0;
        // Sharp corners: the ramp rounds them by up to a pixel.
        let (worst, mean) = compare(&chunk, inside);
        assert!(worst < 0.5 && mean < 0.01, "flip {flip}: {worst} {mean}");
    }
    // The distance itself: exact at known points.
    let d = triangle_distance([0.0, -3.0], [-5.0, 0.0], [5.0, 0.0], [0.0, 5.0]);
    assert!((d - 3.0).abs() < 1e-5, "{d}");
    let d = triangle_distance([0.0, 1.0], [-5.0, 0.0], [5.0, 0.0], [0.0, 5.0]);
    assert!((d + 1.0).abs() < 1e-5, "{d}");
}
