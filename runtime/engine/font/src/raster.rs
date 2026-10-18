//! Glyph rasterization (0012 §6.2): outlines to 8-bit coverage by exact area accumulation.
//!
//! Each line segment adds, to every pixel it crosses, the signed area it sweeps between itself
//! and the pixel's right edge (the font-rs method); a running sum along each row then gives the
//! signed area covered, i.e. the winding-weighted coverage. Quadratic segments are flattened
//! first, finely enough that the flattening error stays far below a level of 8-bit coverage.
//! The coverage is the absolute value clamped to 1: the non-zero rule for the overlapping
//! contours fonts have.

use alloc::vec;
use alloc::vec::Vec;

use marlin_math::scalar::{ceil, floor, sqrt};

use crate::asset::{FontView, Segment};

/// Horizontal subpixel positions a glyph is rasterized at: 0, ¼, ½ and ¾ of a pixel.
pub const SUBPIXEL_POSITIONS: u32 = 4;

/// A glyph's coverage bitmap and where it lies relative to the pen.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    /// The bitmap's top-left pixel relative to the pen position at the baseline (y down), in
    /// whole pixels; the subpixel offset is inside the bitmap.
    pub left: i32,
    pub top: i32,
    /// `width * height` coverage values, rows top to bottom.
    pub coverage: Vec<u8>,
}

/// Accumulates line segments into a `width` × `height` coverage bitmap (pixel units, y down).
pub struct Rasterizer {
    width: usize,
    height: usize,
    /// One more column than the bitmap: the area a segment leaves right of its last pixel.
    acc: Vec<f32>,
}

impl Rasterizer {
    pub fn new(width: u32, height: u32) -> Self {
        let (width, height) = (width as usize, height as usize);
        Self {
            width,
            height,
            acc: vec![0.0; (width + 1) * height],
        }
    }

    fn add(&mut self, x: i32, y: usize, v: f32) {
        // Left of the bitmap counts at its first column; right of it is the spare column.
        let x = x.clamp(0, self.width as i32) as usize;
        self.acc[y * (self.width + 1) + x] += v;
    }

    /// Adds the line from `p0` to `p1`.
    pub fn line(&mut self, p0: [f32; 2], p1: [f32; 2]) {
        if p0[1] == p1[1] {
            return;
        }
        let (dir, p0, p1) = if p0[1] < p1[1] {
            (1.0, p0, p1)
        } else {
            (-1.0, p1, p0)
        };
        let dxdy = (p1[0] - p0[0]) / (p1[1] - p0[1]);
        let y_start = floor(p0[1]).max(0.0) as usize;
        let y_end = (ceil(p1[1]).max(0.0) as usize).min(self.height);
        for y in y_start..y_end {
            let top = (y as f32).max(p0[1]);
            let bottom = ((y + 1) as f32).min(p1[1]);
            let dy = bottom - top;
            if dy <= 0.0 {
                continue;
            }
            let xa = p0[0] + (top - p0[1]) * dxdy;
            let xb = p0[0] + (bottom - p0[1]) * dxdy;
            let d = dy * dir;
            let (x0, x1) = if xa < xb { (xa, xb) } else { (xb, xa) };
            let x0f = floor(x0);
            let x0i = x0f as i32;
            let x1c = ceil(x1);
            let x1i = x1c as i32;
            if x1i <= x0i + 1 {
                // Within one pixel column: its share is the part left of the segment's middle.
                let xm = 0.5 * (xa + xb) - x0f;
                self.add(x0i, y, d * (1.0 - xm));
                self.add(x0i + 1, y, d * xm);
            } else {
                // Across columns: a trapezoid per column, triangles at the ends.
                let s = 1.0 / (x1 - x0);
                let f0 = x0 - x0f;
                let a0 = 0.5 * s * (1.0 - f0) * (1.0 - f0);
                let f1 = x1 - x1c + 1.0;
                let am = 0.5 * s * f1 * f1;
                self.add(x0i, y, d * a0);
                if x1i == x0i + 2 {
                    self.add(x0i + 1, y, d * (1.0 - a0 - am));
                } else {
                    let a1 = s * (1.5 - f0);
                    self.add(x0i + 1, y, d * (a1 - a0));
                    for xi in x0i + 2..x1i - 1 {
                        self.add(xi, y, d * s);
                    }
                    let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                    self.add(x1i - 1, y, d * (1.0 - a2 - am));
                }
                self.add(x1i, y, d * am);
            }
        }
    }

    /// Adds the quadratic from `p0` through `c` to `p1`, flattened.
    pub fn quad(&mut self, p0: [f32; 2], c: [f32; 2], p1: [f32; 2]) {
        let dev = [p0[0] - 2.0 * c[0] + p1[0], p0[1] - 2.0 * c[1] + p1[1]];
        let dd = sqrt(dev[0] * dev[0] + dev[1] * dev[1]);
        // The flattening error of n chords is dd / (8 n²) pixels; keep it under 1/64.
        let n = (1.0 + sqrt(dd * 8.0)) as u32;
        let mut prev = p0;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let p = [
                u * u * p0[0] + 2.0 * u * t * c[0] + t * t * p1[0],
                u * u * p0[1] + 2.0 * u * t * c[1] + t * t * p1[1],
            ];
            self.line(prev, p);
            prev = p;
        }
    }

    /// The coverage of everything added, 0–255 per pixel.
    pub fn coverage(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width * self.height);
        for row in self.acc.chunks_exact(self.width + 1) {
            let mut sum = 0.0;
            for &a in &row[..self.width] {
                sum += a;
                out.push((sum.abs().min(1.0) * 255.0 + 0.5) as u8);
            }
        }
        out
    }
}

/// The pixel bounds of `glyph` at `size` px per em, shifted right by `dx` px (y down, relative
/// to the pen at the baseline): left, top, right, bottom, or `None` for an empty glyph.
pub fn pixel_bounds(font: &FontView<'_>, glyph: u32, size: f32, dx: f32) -> Option<[i32; 4]> {
    let g = font.glyph(glyph);
    if g.contour_count == 0 {
        return None;
    }
    let s = size / font.units_per_em();
    Some([
        floor(f32::from(g.x_min) * s + dx) as i32,
        floor(-f32::from(g.y_max) * s) as i32,
        ceil(f32::from(g.x_max) * s + dx) as i32,
        ceil(-f32::from(g.y_min) * s) as i32,
    ])
}

/// Rasterizes `glyph` at `size` px per em and horizontal subpixel position `subpixel` (in
/// quarters of a pixel, below [`SUBPIXEL_POSITIONS`]). An empty glyph is an empty bitmap.
pub fn rasterize(font: &FontView<'_>, glyph: u32, size: f32, subpixel: u32) -> GlyphBitmap {
    let dx = subpixel as f32 / SUBPIXEL_POSITIONS as f32;
    let Some([left, top, right, bottom]) = pixel_bounds(font, glyph, size, dx) else {
        return GlyphBitmap::default();
    };
    let (width, height) = ((right - left) as u32, (bottom - top) as u32);
    if width == 0 || height == 0 {
        return GlyphBitmap::default();
    }
    let s = size / font.units_per_em();
    let to_px = |p: [f32; 2]| [p[0] * s + dx - left as f32, -p[1] * s - top as f32];
    let mut r = Rasterizer::new(width, height);
    for Segment { p0, c, p1 } in font.segments(glyph) {
        let seg = Segment { p0, c, p1 };
        if seg.is_line() {
            r.line(to_px(p0), to_px(p1));
        } else {
            r.quad(to_px(p0), to_px(c), to_px(p1));
        }
    }
    GlyphBitmap {
        width,
        height,
        left,
        top,
        coverage: r.coverage(),
    }
}
