//! The math of every command, as the shaders do it (0012 §4.1, §5.1): screen bounds, the opaque
//! interior, signed distances, coverage, and a command's premultiplied color at a pixel.
//!
//! `content/engine/shaders/Ui/Ui.mshader` mirrors these functions expression for expression;
//! the CPU reference ([`crate::reference`]) uses them to give the GPU tests their expected
//! values. Bounds use only additions, multiplications by constants, `min`, `max`, `floor` and
//! `ceil`, which are exact on both sides, so the reference's bounds equal the GPU's bit for bit.

use marlin_font::GlyphRecord;
use marlin_math::scalar::{ceil, floor, sqrt};

use crate::command::{CommandWords, IMAGE_SRGB_PREMULTIPLIED, Kind, OPAQUE, PaintWords};
use crate::style::Color;

/// Half a tile's diagonal: how far from its center a tile reaches.
pub const TILE_RADIUS: f32 = 11.313_708;

/// A command's pixels: `bbox` and `interior` are `[x0, y0, x1, y1]`, half-open, clamped to the
/// surface. Nothing outside `bbox` is touched; everything inside `interior` is covered opaquely
/// (empty unless the command is [`OPAQUE`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Bounds {
    pub bbox: [i32; 4],
    pub interior: [i32; 4],
}

impl Bounds {
    pub fn is_empty(&self) -> bool {
        self.bbox[0] >= self.bbox[2] || self.bbox[1] >= self.bbox[3]
    }
}

fn w(payload: &[u32], i: usize) -> f32 {
    f32::from_bits(payload[i])
}

fn pixels(x0: f32, y0: f32, x1: f32, y1: f32, width: u32, height: u32) -> [i32; 4] {
    let (wf, hf) = (width as f32, height as f32);
    [
        floor(x0).clamp(0.0, wf) as i32,
        floor(y0).clamp(0.0, hf) as i32,
        ceil(x1).clamp(0.0, wf) as i32,
        ceil(y1).clamp(0.0, hf) as i32,
    ]
}

/// The bounds of command `c` (its payload at `payload[c[3]..]`) on a `width` × `height` surface.
pub fn bounds(
    c: &CommandWords,
    payload: &[u32],
    glyphs: &[GlyphRecord],
    width: u32,
    height: u32,
) -> Bounds {
    let p = &payload[c[3] as usize..];
    let opaque = c[0] & OPAQUE != 0;
    let none = [0; 4];
    let clamp_i = |v: [f32; 4]| {
        let (wf, hf) = (width as f32, height as f32);
        [
            ceil(v[0]).clamp(0.0, wf) as i32,
            ceil(v[1]).clamp(0.0, hf) as i32,
            floor(v[2]).clamp(0.0, wf) as i32,
            floor(v[3]).clamp(0.0, hf) as i32,
        ]
    };
    match Kind::from_word(c[0]) {
        Some(Kind::Rect) => {
            let e = w(p, 8) * 0.5 + 0.5;
            let bbox = pixels(
                w(p, 0) - e,
                w(p, 1) - e,
                w(p, 2) + e,
                w(p, 3) + e,
                width,
                height,
            );
            let interior = if opaque {
                let r = rect_radius(p).max4();
                clamp_i([w(p, 0) + r, w(p, 1) + r, w(p, 2) - r, w(p, 3) - r])
            } else {
                none
            };
            Bounds { bbox, interior }
        }
        Some(Kind::Circle) => {
            let (cx, cy, r) = (w(p, 0), w(p, 1), w(p, 2));
            let e = r + w(p, 3) * 0.5 + 0.5;
            let bbox = pixels(cx - e, cy - e, cx + e, cy + e, width, height);
            let interior = if opaque {
                let h = (r - 0.5) * core::f32::consts::FRAC_1_SQRT_2;
                clamp_i([cx - h - 0.5, cy - h - 0.5, cx + h + 0.5, cy + h + 0.5])
            } else {
                none
            };
            Bounds { bbox, interior }
        }
        Some(Kind::Line) => {
            let e = line_reach(p) + 0.5;
            let (ax, ay, bx, by) = (w(p, 0), w(p, 1), w(p, 2), w(p, 3));
            Bounds {
                bbox: pixels(
                    ax.min(bx) - e,
                    ay.min(by) - e,
                    ax.max(bx) + e,
                    ay.max(by) + e,
                    width,
                    height,
                ),
                interior: none,
            }
        }
        Some(Kind::Triangle) => {
            let e = w(p, 6) * 0.5 + 0.5;
            let x0 = w(p, 0).min(w(p, 2)).min(w(p, 4));
            let y0 = w(p, 1).min(w(p, 3)).min(w(p, 5));
            let x1 = w(p, 0).max(w(p, 2)).max(w(p, 4));
            let y1 = w(p, 1).max(w(p, 3)).max(w(p, 5));
            Bounds {
                bbox: pixels(x0 - e, y0 - e, x1 + e, y1 + e, width, height),
                interior: none,
            }
        }
        Some(Kind::Glyph) => {
            let Some(g) = glyphs.get(p[2] as usize) else {
                return Bounds::default();
            };
            let (gw, gh) = (g[1] & 0xFFFF, g[1] >> 16);
            let x0 = p[0] as i32 + (g[2] & 0xFFFF) as u16 as i16 as i32;
            let y0 = p[1] as i32 + (g[2] >> 16) as u16 as i16 as i32;
            let clampi = |v: i32, hi: u32| v.clamp(0, hi as i32);
            Bounds {
                bbox: [
                    clampi(x0, width),
                    clampi(y0, height),
                    clampi(x0 + gw as i32, width),
                    clampi(y0 + gh as i32, height),
                ],
                interior: none,
            }
        }
        Some(Kind::Image) => Bounds {
            bbox: pixels(
                w(p, 0) - 0.5,
                w(p, 1) - 0.5,
                w(p, 2) + 0.5,
                w(p, 3) + 0.5,
                width,
                height,
            ),
            interior: none,
        },
        None => Bounds::default(),
    }
}

/// How far a line's ink reaches from its center line.
fn line_reach(p: &[u32]) -> f32 {
    let hw = w(p, 4) * 0.5;
    if p[5] == 2 {
        hw * core::f32::consts::SQRT_2
    } else {
        hw
    }
}

/// Whether the 16 × 16 tile whose center is `(tx, ty)` may hold some of line command `c`'s ink:
/// its center lies within the line's reach plus the tile's (§5.3, the conservative DDA).
pub fn line_touches_tile(p: &[u32], tx: f32, ty: f32) -> f32 {
    let (ax, ay, bx, by) = (w(p, 0), w(p, 1), w(p, 2), w(p, 3));
    let d = segment_distance([tx, ty], [ax, ay], [bx, by]);
    // Negative or zero: touches.
    d - (line_reach(p) + TILE_RADIUS + 0.5)
}

#[derive(Clone, Copy)]
struct Radii([f32; 4]);

impl Radii {
    fn max4(self) -> f32 {
        self.0[0].max(self.0[1]).max(self.0[2]).max(self.0[3])
    }
}

/// A rectangle payload's radii, each at most half the shorter side.
fn rect_radius(p: &[u32]) -> Radii {
    let limit = ((w(p, 2) - w(p, 0)).min(w(p, 3) - w(p, 1)) * 0.5).max(0.0);
    Radii([
        w(p, 4).min(limit),
        w(p, 5).min(limit),
        w(p, 6).min(limit),
        w(p, 7).min(limit),
    ])
}

fn length(x: f32, y: f32) -> f32 {
    sqrt(x * x + y * y)
}

/// Distance from `p` to the segment `a`–`b`.
pub fn segment_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (pax, pay) = (p[0] - a[0], p[1] - a[1]);
    let (bax, bay) = (b[0] - a[0], b[1] - a[1]);
    let len2 = (bax * bax + bay * bay).max(1e-12);
    let h = ((pax * bax + pay * bay) / len2).clamp(0.0, 1.0);
    length(pax - bax * h, pay - bay * h)
}

/// Signed distance to a box of half size `(bx, by)` around the origin, rounded by `r`.
pub fn round_box(px: f32, py: f32, bx: f32, by: f32, r: f32) -> f32 {
    let qx = px.abs() - bx + r;
    let qy = py.abs() - by + r;
    qx.max(qy).min(0.0) + length(qx.max(0.0), qy.max(0.0)) - r
}

fn rect_distance(p: &[u32], x: f32, y: f32) -> f32 {
    let cx = (w(p, 0) + w(p, 2)) * 0.5;
    let cy = (w(p, 1) + w(p, 3)) * 0.5;
    let bx = (w(p, 2) - w(p, 0)) * 0.5;
    let by = (w(p, 3) - w(p, 1)) * 0.5;
    let (px, py) = (x - cx, y - cy);
    let r = rect_radius(p).0;
    // top-left, top-right, bottom-right, bottom-left; y down.
    let radius = if px > 0.0 {
        if py > 0.0 { r[2] } else { r[1] }
    } else if py > 0.0 {
        r[3]
    } else {
        r[0]
    };
    round_box(px, py, bx, by, radius)
}

/// Signed distance to triangle `a b c` (either winding), negative inside.
pub fn triangle_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    let e0 = [b[0] - a[0], b[1] - a[1]];
    let e1 = [c[0] - b[0], c[1] - b[1]];
    let e2 = [a[0] - c[0], a[1] - c[1]];
    let v0 = [p[0] - a[0], p[1] - a[1]];
    let v1 = [p[0] - b[0], p[1] - b[1]];
    let v2 = [p[0] - c[0], p[1] - c[1]];
    let dot = |u: [f32; 2], v: [f32; 2]| u[0] * v[0] + u[1] * v[1];
    let pq = |v: [f32; 2], e: [f32; 2]| {
        let h = (dot(v, e) / dot(e, e).max(1e-12)).clamp(0.0, 1.0);
        [v[0] - e[0] * h, v[1] - e[1] * h]
    };
    let (q0, q1, q2) = (pq(v0, e0), pq(v1, e1), pq(v2, e2));
    let s = if e0[0] * e2[1] - e0[1] * e2[0] < 0.0 {
        -1.0
    } else {
        1.0
    };
    let dx = dot(q0, q0).min(dot(q1, q1)).min(dot(q2, q2));
    let dy = (s * (v0[0] * e0[1] - v0[1] * e0[0]))
        .min(s * (v1[0] * e1[1] - v1[1] * e1[0]))
        .min(s * (v2[0] * e2[1] - v2[1] * e2[0]));
    let mag = sqrt(dx);
    if dy > 0.0 { -mag } else { mag }
}

/// Signed distance to a line command's ink: its caps and dashes.
fn line_distance(p: &[u32], x: f32, y: f32) -> f32 {
    let (ax, ay, bx, by) = (w(p, 0), w(p, 1), w(p, 2), w(p, 3));
    let hw = w(p, 4) * 0.5;
    let (dx, dy) = (bx - ax, by - ay);
    let len = length(dx, dy);
    let (ux, uy) = if len > 1e-6 {
        (dx / len, dy / len)
    } else {
        (1.0, 0.0)
    };
    let (rx, ry) = (x - ax, y - ay);
    let u = rx * ux + ry * uy;
    let v = rx * uy - ry * ux;
    let mut d = if p[5] == 1 {
        let t = u.clamp(0.0, len);
        length(u - t, v) - hw
    } else {
        let ext = if p[5] == 2 { hw } else { 0.0 };
        let qx = (u - len * 0.5).abs() - (len * 0.5 + ext);
        let qy = v.abs() - hw;
        length(qx.max(0.0), qy.max(0.0)) + qx.max(qy).min(0.0)
    };
    let dash = w(p, 6);
    if dash > 0.0 {
        let period = dash + w(p, 7);
        let s = u + w(p, 8) - dash * 0.5 + period * 0.5;
        let t = s - period * floor(s / period) - period * 0.5;
        d = d.max(t.abs() - dash * 0.5);
    }
    d
}

/// Coverage of a pixel whose center is at signed distance `d` (§4.1): one pixel of transition.
pub fn coverage(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

/// A packed sRGB color with straight alpha as premultiplied floats.
pub fn premultiplied(packed: u32) -> [f32; 4] {
    let c = Color::from_packed(packed);
    let a = c.a as f32 / 255.0;
    [
        c.r as f32 / 255.0 * a,
        c.g as f32 / 255.0 * a,
        c.b as f32 / 255.0 * a,
        a,
    ]
}

fn scale4(c: [f32; 4], k: f32) -> [f32; 4] {
    [c[0] * k, c[1] * k, c[2] * k, c[3] * k]
}

/// `src` over `dst`, premultiplied.
pub fn over(src: [f32; 4], dst: [f32; 4]) -> [f32; 4] {
    let k = 1.0 - src[3];
    [
        src[0] + dst[0] * k,
        src[1] + dst[1] * k,
        src[2] + dst[2] * k,
        src[3] + dst[3] * k,
    ]
}

/// The sRGB encoding of linear `x`.
pub fn linear_to_srgb(x: f32) -> f32 {
    if x <= 0.003_130_8 {
        x * 12.92
    } else {
        1.055 * marlin_math::scalar::powf(x, 1.0 / 2.4) - 0.055
    }
}

pub fn srgb_to_linear(x: f32) -> f32 {
    if x <= 0.040_45 {
        x / 12.92
    } else {
        marlin_math::scalar::powf((x + 0.055) / 1.055, 2.4)
    }
}

/// Fill and outline coverage composed: `fill` color under the stroke.
fn fill_and_stroke(fill: [f32; 4], d: f32, stroke_width: f32, stroke: u32) -> [f32; 4] {
    let f = scale4(fill, coverage(d));
    if stroke_width <= 0.0 {
        return f;
    }
    let s = scale4(
        premultiplied(stroke),
        coverage(d.abs() - stroke_width * 0.5),
    );
    over(s, f)
}

/// What the shader samples images and glyphs from.
pub trait Textures {
    /// The glyph page texel at (`x`, `y`) of `page`, 0–1.
    fn glyph(&self, page: u32, x: u32, y: u32) -> f32;
    /// Texture `view` through `sampler` at `uv`, mip `lod`.
    fn sample(&self, view: u32, sampler: u32, uv: [f32; 2], lod: f32) -> [f32; 4];
}

/// Command `c`'s premultiplied color (sRGB-encoded) at the pixel whose center is `(x, y)`.
pub fn shade(
    c: &CommandWords,
    payload: &[u32],
    paints: &[PaintWords],
    glyphs: &[GlyphRecord],
    textures: &dyn Textures,
    x: f32,
    y: f32,
) -> [f32; 4] {
    let p = &payload[c[3] as usize..];
    let paint = premultiplied(paints[c[2] as usize][1]);
    match Kind::from_word(c[0]) {
        Some(Kind::Rect) => fill_and_stroke(paint, rect_distance(p, x, y), w(p, 8), p[9]),
        Some(Kind::Circle) => {
            let d = length(x - w(p, 0), y - w(p, 1)) - w(p, 2);
            fill_and_stroke(paint, d, w(p, 3), p[4])
        }
        Some(Kind::Line) => scale4(paint, coverage(line_distance(p, x, y))),
        Some(Kind::Triangle) => {
            let d = triangle_distance(
                [x, y],
                [w(p, 0), w(p, 1)],
                [w(p, 2), w(p, 3)],
                [w(p, 4), w(p, 5)],
            );
            fill_and_stroke(paint, d, w(p, 6), p[7])
        }
        Some(Kind::Glyph) => {
            let Some(g) = glyphs.get(p[2] as usize) else {
                return [0.0; 4];
            };
            let (gw, gh) = ((g[1] & 0xFFFF) as i32, (g[1] >> 16) as i32);
            let gx = floor(x) as i32 - (p[0] as i32 + (g[2] & 0xFFFF) as u16 as i16 as i32);
            let gy = floor(y) as i32 - (p[1] as i32 + (g[2] >> 16) as u16 as i16 as i32);
            if gx < 0 || gy < 0 || gx >= gw || gy >= gh {
                return [0.0; 4];
            }
            let t = textures.glyph(g[3], (g[0] & 0xFFFF) + gx as u32, (g[0] >> 16) + gy as u32);
            scale4(paint, t)
        }
        Some(Kind::Image) => {
            let r = rect_radius(p).0;
            let rect = [p[0], p[1], p[2], p[3], f(r[0]), f(r[1]), f(r[2]), f(r[3])];
            let cov = coverage(rect_distance(&rect, x, y));
            if cov <= 0.0 {
                return [0.0; 4];
            }
            let (x0, y0, x1, y1) = (w(p, 0), w(p, 1), w(p, 2), w(p, 3));
            let (u0, v0, u1, v1) = (w(p, 4), w(p, 5), w(p, 6), w(p, 7));
            let du = (u1 - u0) / (x1 - x0);
            let dv = (v1 - v0) / (y1 - y0);
            let uv = [u0 + (x - x0) * du, v0 + (y - y0) * dv];
            let (tw, th) = ((p[10] & 0xFFFF) as f32, (p[10] >> 16) as f32);
            let texels = (du.abs() * tw).max(dv.abs() * th);
            let lod = marlin_math::scalar::log2(texels.max(1e-6)).max(0.0);
            let mut t = textures.sample(p[8], p[9], uv, lod);
            if c[0] & IMAGE_SRGB_PREMULTIPLIED == 0 {
                let a = t[3];
                t = [
                    linear_to_srgb(t[0]) * a,
                    linear_to_srgb(t[1]) * a,
                    linear_to_srgb(t[2]) * a,
                    a,
                ];
            }
            let tinted = [
                t[0] * paint[0],
                t[1] * paint[1],
                t[2] * paint[2],
                t[3] * paint[3],
            ];
            // `paint` is premultiplied: the tint's alpha is in its color too, so the color
            // takes the texel's color times the tint's premultiplied color.
            scale4(tinted, cov)
        }
        None => [0.0; 4],
    }
}

fn f(v: f32) -> u32 {
    v.to_bits()
}
