//! Path fill (§4.2): coverage under both fill rules against supersampled shapes, curves
//! flattened within tolerance, rows of bands, and replays moving paths.

use marlin_font::GlyphIds;
use marlin_math::Vec2;
use marlin_ui_render::command::Chunk;
use marlin_ui_render::path::FLATTEN_TOLERANCE;
use marlin_ui_render::shape::{Scene, Textures, shade};
use marlin_ui_render::{
    Affine2, Color, FillRule, GroupDesc, Paint, Painter, Path, RetainedChunk, UiDrawList,
};

use crate::recording::resolved;

struct NoTextures;

impl Textures for NoTextures {
    fn glyph(&self, _: u32, _: u32, _: u32) -> f32 {
        0.0
    }
    fn sample(&self, _: u32, _: u32, _: [f32; 2], _: f32) -> [f32; 4] {
        [0.0; 4]
    }
}

fn record(f: impl FnOnce(&mut Painter<'_>)) -> Chunk {
    let ids = GlyphIds::new();
    let mut list = UiDrawList::new(1.0);
    f(&mut list.painter(&ids));
    list.flatten()
}

/// Winding number of the closed polygon `pts` around `(x, y)`.
fn winding(pts: &[Vec2], x: f32, y: f32) -> i32 {
    let mut w = 0;
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        if (a.y <= y) != (b.y <= y) {
            let t = (y - a.y) / (b.y - a.y);
            if a.x + t * (b.x - a.x) > x {
                w += if b.y > a.y { 1 } else { -1 };
            }
        }
    }
    w
}

/// The path command's alpha against `inside` supersampled 16 × 16 over a `size` square.
fn compare(chunk: &Chunk, size: u32, inside: impl Fn(f32, f32) -> bool) -> (f32, f32) {
    let n = 16;
    let scene = Scene::new(chunk, &[]);
    let (mut worst, mut sum) = (0.0f32, 0.0);
    for py in 0..size {
        for px in 0..size {
            let a = shade(
                &chunk.commands[0],
                &scene,
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
    (worst, sum / (size * size) as f32)
}

fn star() -> Vec<Vec2> {
    (0..5)
        .map(|i| {
            let a = -core::f32::consts::FRAC_PI_2 + i as f32 * 4.0 * core::f32::consts::PI / 5.0;
            Vec2::new(40.0 + 34.0 * a.cos(), 42.0 + 34.0 * a.sin())
        })
        .collect()
}

fn polygon(pts: &[Vec2]) -> Path {
    let mut p = Path::new();
    p.move_to(pts[0]);
    for &q in &pts[1..] {
        p.line_to(q);
    }
    p.close();
    p
}

#[test]
fn a_star_fills_by_either_rule() {
    let pts = star();
    for (rule, inside) in [
        (
            FillRule::NonZero,
            Box::new(|w: i32| w != 0) as Box<dyn Fn(i32) -> bool>,
        ),
        (FillRule::EvenOdd, Box::new(|w: i32| w % 2 != 0)),
    ] {
        let chunk = record(|p| p.fill_path(&polygon(&pts), rule, &Paint::Solid(Color::WHITE)));
        let (worst, mean) = compare(&chunk, 80, |x, y| inside(winding(&pts, x, y)));
        // Exact areas, but a pixel where a winding of 2 meets the outside counts its covered
        // part twice (signed areas sum windings), and even-odd folds that sum: only on the
        // star's crossing edges.
        assert!(worst < 0.5 && mean < 0.002, "{rule:?}: {worst} {mean}");
    }
}

#[test]
fn contours_add_their_windings() {
    // An outer square and an inner one wound the other way: a hole either way; wound the same
    // way: filled for non-zero, a hole for even-odd.
    let outer = [
        Vec2::new(4.0, 4.0),
        Vec2::new(60.0, 4.0),
        Vec2::new(60.0, 60.0),
        Vec2::new(4.0, 60.0),
    ];
    let inner_cw = [
        Vec2::new(20.0, 20.5),
        Vec2::new(44.25, 20.5),
        Vec2::new(44.25, 44.0),
        Vec2::new(20.0, 44.0),
    ];
    let inner_ccw: Vec<Vec2> = inner_cw.iter().rev().copied().collect();
    let path = |inner: &[Vec2]| {
        let mut p = polygon(&outer);
        p.move_to(inner[0]);
        for &q in &inner[1..] {
            p.line_to(q);
        }
        p.close();
        p
    };
    for (inner, rule, hole) in [
        (&inner_ccw[..], FillRule::NonZero, true),
        (&inner_cw[..], FillRule::NonZero, false),
        (&inner_cw[..], FillRule::EvenOdd, true),
    ] {
        let chunk = record(|p| p.fill_path(&path(inner), rule, &Paint::Solid(Color::WHITE)));
        let (worst, _) = compare(&chunk, 64, |x, y| {
            let o = winding(&outer, x, y);
            let i = winding(inner, x, y);
            let w = o + i;
            match rule {
                FillRule::NonZero => w != 0,
                FillRule::EvenOdd => w % 2 != 0,
            }
        });
        assert!(worst < 0.01, "{rule:?} hole {hole}: {worst}");
        let a = shade(
            &chunk.commands[0],
            &Scene::new(&chunk, &[]),
            &NoTextures,
            32.5,
            32.5,
        )[3];
        assert_eq!(a, if hole { 0.0 } else { 1.0 });
    }
}

#[test]
fn curves_flatten_within_tolerance() {
    // A circle of four cubic arcs, in a scaled and turned group.
    let k = 0.552_284_8;
    let r = 12.0;
    let mut path = Path::new();
    path.move_to(Vec2::new(r, 0.0))
        .cubic_to(Vec2::new(r, r * k), Vec2::new(r * k, r), Vec2::new(0.0, r))
        .cubic_to(
            Vec2::new(-r * k, r),
            Vec2::new(-r, r * k),
            Vec2::new(-r, 0.0),
        )
        .cubic_to(
            Vec2::new(-r, -r * k),
            Vec2::new(-r * k, -r),
            Vec2::new(0.0, -r),
        )
        .cubic_to(
            Vec2::new(r * k, -r),
            Vec2::new(r, -r * k),
            Vec2::new(r, 0.0),
        )
        .close();
    let t = Affine2::translate(32.0, 32.0) * Affine2::rotate(0.4) * Affine2::scale(2.25, 2.25);
    let chunk = record(|p| {
        p.begin_group(&GroupDesc {
            transform: t,
            ..GroupDesc::default()
        });
        p.fill_path(&path, FillRule::NonZero, &Paint::Solid(Color::WHITE));
        p.end_group();
    });
    // The cubic circle strays 0.027 % of r from a true circle: 0.007 px here.
    let (worst, mean) = compare(&chunk, 64, |x, y| (x - 32.0).hypot(y - 32.0) <= r * 2.25);
    assert!(worst < 0.05 && mean < 0.002, "{worst} {mean}");
    // Segments short enough for the tolerance: no chord strays more than it.
    let segs = path.flatten(t);
    for s in &segs {
        let mid = Vec2::new((s[0] + s[2]) * 0.5, (s[1] + s[3]) * 0.5);
        let off = ((mid - Vec2::new(32.0, 32.0)).length() - r * 2.25).abs();
        assert!(off < FLATTEN_TOLERANCE + 0.01, "{off}");
    }
    // A quadratic: one segment when it is straight.
    let mut q = Path::new();
    q.move_to(Vec2::ZERO)
        .quad_to(Vec2::new(5.0, 5.0), Vec2::new(10.0, 10.0));
    assert_eq!(
        q.flatten(Affine2::IDENTITY).len(),
        2,
        "the curve and the closing line"
    );
}

#[test]
fn tall_paths_span_bands() {
    // A thin diagonal sliver from top to bottom crosses every band.
    let pts = [
        Vec2::new(10.0, 2.0),
        Vec2::new(14.0, 2.0),
        Vec2::new(54.0, 126.0),
        Vec2::new(50.0, 126.0),
    ];
    let chunk = record(|p| {
        p.fill_path(
            &polygon(&pts),
            FillRule::NonZero,
            &Paint::Solid(Color::WHITE),
        )
    });
    let p = &chunk.payload[chunk.commands[0][3] as usize..];
    assert_eq!(p[3], 8, "126 rows from y = 2: 8 bands of 16");
    let (worst, _) = compare(&chunk, 128, |x, y| winding(&pts, x, y) != 0);
    assert!(worst < 0.05, "{worst}");
}

#[test]
fn replays_move_paths() {
    let ids = GlyphIds::new();
    let draw = |p: &mut Painter<'_>, at: Vec2| {
        let pts: Vec<Vec2> = star().iter().map(|&q| q * 0.5 + at).collect();
        p.fill_path(
            &polygon(&pts),
            FillRule::EvenOdd,
            &Paint::Solid(Color::rgb(200, 30, 30)),
        );
    };
    let retained = RetainedChunk::record(1.0, &ids, |p| draw(p, Vec2::new(3.0, 4.0)));
    let mut replayed = UiDrawList::new(1.0);
    replayed
        .painter(&ids)
        .replay(&retained, Vec2::new(7.0, 13.0));
    let mut direct = UiDrawList::new(1.0);
    draw(&mut direct.painter(&ids), Vec2::new(10.0, 17.0));
    // The same coverage everywhere (the bands' origin moved with the path).
    let (a, b) = (replayed.flatten(), direct.flatten());
    assert_eq!(resolved(&a).len(), 1);
    for y in 0..64 {
        for x in 0..64 {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let ca = shade(&a.commands[0], &Scene::new(&a, &[]), &NoTextures, fx, fy)[3];
            let cb = shade(&b.commands[0], &Scene::new(&b, &[]), &NoTextures, fx, fy)[3];
            assert!((ca - cb).abs() < 1e-4, "({x}, {y}): {ca} {cb}");
        }
    }
}
