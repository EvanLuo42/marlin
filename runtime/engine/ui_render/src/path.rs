//! Paths (§4.2): contours of lines, quadratic and cubic Béziers, filled by the non-zero or the
//! even-odd rule.
//!
//! The painter flattens a path into line segments on the screen when it records it (curves to
//! within [`FLATTEN_TOLERANCE`] of a pixel), and indexes the segments by 16-pixel rows. A pixel's
//! coverage is the exact signed area the segments in its row sweep to its right: segments left
//! of the pixel count fully, as the backdrop does in a tile renderer, so no separate pass sums
//! winding numbers.

use alloc::vec::Vec;

use marlin_math::Vec2;
use marlin_math::scalar::{ceil, floor, sqrt};

use crate::style::Affine2;

/// How far flattened curves stray from the true curve, in screen pixels.
pub const FLATTEN_TOLERANCE: f32 = 0.05;
/// Rows of pixels a band holds.
pub const BAND: f32 = 16.0;

/// Which points a path covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FillRule {
    /// Where the winding number is not zero.
    #[default]
    NonZero,
    /// Where the winding number is odd.
    EvenOdd,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Verb {
    Move(Vec2),
    Line(Vec2),
    Quad(Vec2, Vec2),
    Cubic(Vec2, Vec2, Vec2),
    Close,
}

/// Contours in logical pixels. Every contour is closed when filled.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Path {
    verbs: Vec<Verb>,
}

impl Path {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn move_to(&mut self, p: Vec2) -> &mut Self {
        self.verbs.push(Verb::Move(p));
        self
    }

    pub fn line_to(&mut self, p: Vec2) -> &mut Self {
        self.verbs.push(Verb::Line(p));
        self
    }

    pub fn quad_to(&mut self, c: Vec2, p: Vec2) -> &mut Self {
        self.verbs.push(Verb::Quad(c, p));
        self
    }

    pub fn cubic_to(&mut self, c1: Vec2, c2: Vec2, p: Vec2) -> &mut Self {
        self.verbs.push(Verb::Cubic(c1, c2, p));
        self
    }

    pub fn close(&mut self) -> &mut Self {
        self.verbs.push(Verb::Close);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.verbs.is_empty()
    }

    /// The path's segments through `to_screen`, curves flattened, every contour closed.
    pub fn flatten(&self, to_screen: Affine2) -> Vec<[f32; 4]> {
        let mut out = Vec::new();
        let (mut start, mut at) = (Vec2::ZERO, Vec2::ZERO);
        let mut open = false;
        let line = |out: &mut Vec<[f32; 4]>, a: Vec2, b: Vec2| {
            if a != b {
                out.push([a.x, a.y, b.x, b.y]);
            }
        };
        let tf = |p: Vec2| to_screen.apply(p);
        for v in &self.verbs {
            match *v {
                Verb::Move(p) => {
                    if open {
                        line(&mut out, at, start);
                    }
                    start = tf(p);
                    at = start;
                    open = true;
                }
                Verb::Line(p) => {
                    let p = tf(p);
                    line(&mut out, at, p);
                    at = p;
                    open = true;
                }
                Verb::Quad(c, p) => {
                    let (c, p) = (tf(c), tf(p));
                    let dd = (at - c * 2.0 + p).length();
                    let n = ceil(sqrt(dd / (8.0 * FLATTEN_TOLERANCE))).max(1.0) as u32;
                    let p0 = at;
                    for i in 1..=n {
                        let t = i as f32 / n as f32;
                        let u = 1.0 - t;
                        let q = p0 * (u * u) + c * (2.0 * u * t) + p * (t * t);
                        line(&mut out, at, q);
                        at = q;
                    }
                    at = p;
                    open = true;
                }
                Verb::Cubic(c1, c2, p) => {
                    let (c1, c2, p) = (tf(c1), tf(c2), tf(p));
                    let p0 = at;
                    let dd = (p0 - c1 * 2.0 + c2)
                        .length()
                        .max((c1 - c2 * 2.0 + p).length());
                    let n = ceil(sqrt(0.75 * dd / FLATTEN_TOLERANCE)).max(1.0) as u32;
                    for i in 1..=n {
                        let t = i as f32 / n as f32;
                        let u = 1.0 - t;
                        let q = p0 * (u * u * u)
                            + c1 * (3.0 * u * u * t)
                            + c2 * (3.0 * u * t * t)
                            + p * (t * t * t);
                        line(&mut out, at, q);
                        at = q;
                    }
                    at = p;
                    open = true;
                }
                Verb::Close => {
                    if open {
                        line(&mut out, at, start);
                    }
                    at = start;
                    open = false;
                }
            }
        }
        if open {
            line(&mut out, at, start);
        }
        out
    }
}

/// A path command's payload for screen segments `segs` (see `crate::command`), or `None` for a
/// path with no area.
pub(crate) fn payload(segs: &[[f32; 4]], rule: FillRule) -> Option<Vec<u32>> {
    // Horizontal segments sweep no area.
    let segs: Vec<[f32; 4]> = segs.iter().copied().filter(|s| s[1] != s[3]).collect();
    if segs.is_empty() {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for s in &segs {
        x0 = x0.min(s[0]).min(s[2]);
        y0 = y0.min(s[1]).min(s[3]);
        x1 = x1.max(s[0]).max(s[2]);
        y1 = y1.max(s[1]).max(s[3]);
    }
    let origin = floor(y0);
    let bands = (floor((y1 - origin) / BAND) as usize) + 1;
    let mut lists: Vec<Vec<u32>> = alloc::vec![Vec::new(); bands];
    for (i, s) in segs.iter().enumerate() {
        let lo = floor((s[1].min(s[3]) - origin) / BAND) as usize;
        let hi = (floor((s[1].max(s[3]) - origin) / BAND) as usize).min(bands - 1);
        for list in &mut lists[lo..=hi] {
            list.push(i as u32);
        }
    }
    let indices: usize = lists.iter().map(Vec::len).sum();
    let len = 9 + 2 * bands + indices + 4 * segs.len();
    let mut p = Vec::with_capacity(len);
    p.extend_from_slice(&[
        match rule {
            FillRule::NonZero => 0,
            FillRule::EvenOdd => 1,
        },
        len as u32,
        origin.to_bits(),
        bands as u32,
        segs.len() as u32,
        x0.to_bits(),
        y0.to_bits(),
        x1.to_bits(),
        y1.to_bits(),
    ]);
    let mut at = 9 + 2 * bands;
    for l in &lists {
        p.push(at as u32);
        p.push(l.len() as u32);
        at += l.len();
    }
    for l in &lists {
        p.extend_from_slice(l);
    }
    for s in &segs {
        p.extend(s.iter().map(|v| v.to_bits()));
    }
    debug_assert_eq!(p.len(), len);
    Some(p)
}

/// The signed area segment `s` sweeps to the right inside pixel cell `(px, py)` (its top-left
/// corner): what the cell's coverage sums.
pub fn cell_area(s: [f32; 4], px: f32, py: f32) -> f32 {
    let (x0, y0, x1, y1) = (s[0], s[1], s[2], s[3]);
    if y0 == y1 {
        return 0.0;
    }
    let dir = if y1 > y0 { 1.0 } else { -1.0 };
    let ya = y0.min(y1).max(py);
    let yb = y0.max(y1).min(py + 1.0);
    if yb <= ya {
        return 0.0;
    }
    let dxdy = (x1 - x0) / (y1 - y0);
    let ta = x0 + (ya - y0) * dxdy - px;
    let tb = x0 + (yb - y0) * dxdy - px;
    // ∫ clamp(1 - t, 0, 1) dt: t for t ≤ 0, t - t²/2 between, ½ beyond 1.
    let g = |t: f32| {
        if t <= 0.0 {
            t
        } else if t >= 1.0 {
            0.5
        } else {
            t - t * t * 0.5
        }
    };
    let h = |t: f32| (1.0 - t).clamp(0.0, 1.0);
    let avg = if (tb - ta).abs() < 1e-6 {
        h(ta)
    } else {
        (g(tb) - g(ta)) / (tb - ta)
    };
    dir * avg * (yb - ya)
}
