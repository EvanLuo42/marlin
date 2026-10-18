//! The rasterizer against exact areas and against supersampling of the same outlines.

use marlin_font::raster::{Rasterizer, pixel_bounds};
use marlin_font::{Segment, rasterize};

use crate::inter;

/// Coverage of a polygon by `n`×`n` samples per pixel with the non-zero rule: the reference.
fn supersample(edges: &[([f32; 2], [f32; 2])], width: u32, height: u32, n: u32) -> Vec<f32> {
    let mut out = vec![0.0; (width * height) as usize];
    for py in 0..height {
        for px in 0..width {
            let mut inside = 0;
            for sy in 0..n {
                for sx in 0..n {
                    let x = px as f32 + (sx as f32 + 0.5) / n as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / n as f32;
                    let mut winding = 0;
                    for &(a, b) in edges {
                        if (a[1] <= y) != (b[1] <= y) {
                            let t = (y - a[1]) / (b[1] - a[1]);
                            if a[0] + t * (b[0] - a[0]) > x {
                                winding += if b[1] > a[1] { 1 } else { -1 };
                            }
                        }
                    }
                    if winding != 0 {
                        inside += 1;
                    }
                }
            }
            out[(py * width + px) as usize] = inside as f32 / (n * n) as f32;
        }
    }
    out
}

/// `segments` flattened into many chords.
fn polygon(segments: &[Segment]) -> Vec<([f32; 2], [f32; 2])> {
    let mut edges = Vec::new();
    for s in segments {
        let steps = if s.is_line() { 1 } else { 64 };
        let at = |t: f32| {
            let u = 1.0 - t;
            [
                u * u * s.p0[0] + 2.0 * u * t * s.c[0] + t * t * s.p1[0],
                u * u * s.p0[1] + 2.0 * u * t * s.c[1] + t * t * s.p1[1],
            ]
        };
        for i in 0..steps {
            edges.push((
                at(i as f32 / steps as f32),
                at((i + 1) as f32 / steps as f32),
            ));
        }
    }
    edges
}

#[test]
fn a_rectangle_covers_its_exact_area() {
    // x from 1.25 to 4.5, y from 0.5 to 2.75, clockwise in y-down pixels.
    let mut r = Rasterizer::new(6, 4);
    let (x0, y0, x1, y1) = (1.25, 0.5, 4.5, 2.75);
    r.line([x0, y0], [x1, y0]);
    r.line([x1, y0], [x1, y1]);
    r.line([x1, y1], [x0, y1]);
    r.line([x0, y1], [x0, y0]);
    let got = r.coverage();
    for y in 0..4 {
        for x in 0..6 {
            let cx = (x1.min(x as f32 + 1.0) - x0.max(x as f32)).max(0.0);
            let cy = (y1.min(y as f32 + 1.0) - y0.max(y as f32)).max(0.0);
            let expected = (cx * cy * 255.0 + 0.5) as u8;
            assert_eq!(got[y * 6 + x], expected, "pixel ({x}, {y})");
        }
    }
}

#[test]
fn slanted_edges_match_supersampling() {
    // A triangle with edges at assorted slopes, some crossing several columns per row.
    let pts = [[0.3, 0.2], [11.7, 3.1], [4.2, 9.6]];
    let mut r = Rasterizer::new(12, 10);
    let mut edges = Vec::new();
    for i in 0..3 {
        r.line(pts[i], pts[(i + 1) % 3]);
        edges.push((pts[i], pts[(i + 1) % 3]));
    }
    let got = r.coverage();
    let reference = supersample(&edges, 12, 10, 64);
    for (i, (&g, &e)) in got.iter().zip(&reference).enumerate() {
        let diff = (g as f32 / 255.0 - e).abs();
        assert!(diff < 0.02, "pixel {i}: {g} vs {e}");
    }
}

/// `glyph`'s outline in the bitmap's pixel space, as [`rasterize`] places it.
fn placed(glyph: u32, size: f32, subpixel: u32) -> (Vec<Segment>, u32, u32) {
    let font = inter();
    let dx = subpixel as f32 / 4.0;
    let [left, top, right, bottom] = pixel_bounds(&font, glyph, size, dx).unwrap();
    let s = size / font.units_per_em();
    let f = |p: [f32; 2]| [p[0] * s + dx - left as f32, -p[1] * s - top as f32];
    let segs = font
        .segments(glyph)
        .map(|g| Segment {
            p0: f(g.p0),
            c: f(g.c),
            p1: f(g.p1),
        })
        .collect();
    (segs, (right - left) as u32, (bottom - top) as u32)
}

#[test]
fn inter_glyphs_match_supersampling() {
    let font = inter();
    for (c, size) in [
        ('O', 24.0),
        ('g', 15.0),
        ('&', 31.5),
        ('é', 12.0),
        ('S', 36.0),
    ] {
        let glyph = font.glyph_of(c).unwrap();
        for subpixel in [0, 3] {
            let bitmap = rasterize(&font, glyph, size, subpixel);
            let (segs, w, h) = placed(glyph, size, subpixel);
            assert_eq!((bitmap.width, bitmap.height), (w, h));
            let reference = supersample(&polygon(&segs), w, h, 16);
            let mut worst = 0.0f32;
            let mut total = 0.0;
            for (&g, &e) in bitmap.coverage.iter().zip(&reference) {
                let d = (g as f32 / 255.0 - e).abs();
                worst = worst.max(d);
                total += d;
            }
            // 16² samples resolve 1/256 of a pixel; flattening and sampling noise stay small.
            assert!(
                worst < 0.06,
                "{c:?} at {size} px, subpixel {subpixel}: worst {worst}"
            );
            let mean = total / reference.len() as f32;
            assert!(mean < 0.01, "{c:?}: mean {mean}");
        }
    }
}

#[test]
fn subpixel_positions_shift_the_glyph() {
    // Inter's 'l' is a rectangle: a middle row's coverage is the box-filtered stem, moved by
    // the subpixel position.
    let font = inter();
    let glyph = font.glyph_of('l').unwrap();
    let xs: Vec<f32> = font.segments(glyph).map(|s| s.p0[0]).collect();
    let (x0, x1) = (
        xs.iter().cloned().fold(f32::MAX, f32::min),
        xs.iter().cloned().fold(f32::MIN, f32::max),
    );
    let size = 20.0;
    let s = size / font.units_per_em();
    for q in 0..4 {
        let dx = q as f32 / 4.0;
        let b = rasterize(&font, glyph, size, q);
        let row = &b.coverage[(b.height / 2 * b.width) as usize..][..b.width as usize];
        for (i, &c) in row.iter().enumerate() {
            let px = (b.left + i as i32) as f32;
            let covered = ((x1 * s + dx).min(px + 1.0) - (x0 * s + dx).max(px)).clamp(0.0, 1.0);
            assert_eq!(c, (covered * 255.0 + 0.5) as u8, "subpixel {q}, column {i}");
        }
    }
}

#[test]
fn empty_glyphs_are_empty_bitmaps() {
    let font = inter();
    let space = font.glyph_of(' ').unwrap();
    let b = rasterize(&font, space, 16.0, 0);
    assert_eq!((b.width, b.height, b.coverage.len()), (0, 0, 0));
}
