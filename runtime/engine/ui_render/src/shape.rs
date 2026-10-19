//! The math of every command, as the shaders do it (0012 §3.2, §4.1, §5.1): screen bounds, the
//! opaque interior, signed distances, coverage, and a command's premultiplied color at a pixel,
//! through its group's transform and its clip chain.
//!
//! `content/engine/shaders/Ui/Ui.mshader` mirrors these functions expression for expression;
//! the CPU reference ([`crate::reference`]) uses them to give the GPU tests their expected
//! values. Bounds use only additions, multiplications, `min`, `max`, `floor` and `ceil`; with
//! transforms whose products are exact (the identity, scales by powers of two, quarter-pixel
//! translations) the reference's bounds equal the GPU's bit for bit.

use marlin_font::GlyphRecord;
use marlin_math::scalar::{ceil, floor, sqrt};

use crate::command::{
    Chunk, ClipWords, CommandWords, GROUP_AXIS_ALIGNED, GROUP_NONUNIFORM, GroupWords,
    IMAGE_SRGB_PREMULTIPLIED, Kind, OPAQUE, PaintWords, clip_of, group_of,
};
use crate::style::Color;

/// Half a tile's diagonal: how far from its center a tile reaches.
pub const TILE_RADIUS: f32 = 11.313_708;

/// What commands refer to: the frame's commands, payload, paints, groups, clips and glyph
/// table.
#[derive(Clone, Copy)]
pub struct Scene<'a> {
    /// Every command: group effects read their members.
    pub commands: &'a [CommandWords],
    pub payload: &'a [u32],
    pub paints: &'a [PaintWords],
    pub groups: &'a [GroupWords],
    pub clips: &'a [ClipWords],
    pub glyphs: &'a [GlyphRecord],
}

impl<'a> Scene<'a> {
    pub fn new(chunk: &'a Chunk, glyphs: &'a [GlyphRecord]) -> Self {
        Self {
            commands: &chunk.commands,
            payload: &chunk.payload,
            paints: &chunk.paints,
            groups: &chunk.groups,
            clips: &chunk.clips,
            glyphs,
        }
    }
}

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

/// The floor/ceil of a box, clamped to the surface.
fn pixels(x0: f32, y0: f32, x1: f32, y1: f32, width: u32, height: u32) -> [i32; 4] {
    let (wf, hf) = (width as f32, height as f32);
    [
        floor(x0).clamp(0.0, wf) as i32,
        floor(y0).clamp(0.0, hf) as i32,
        ceil(x1).clamp(0.0, wf) as i32,
        ceil(y1).clamp(0.0, hf) as i32,
    ]
}

/// The ceil/floor of a box, clamped: the whole pixels inside it.
fn inside(x0: f32, y0: f32, x1: f32, y1: f32, width: u32, height: u32) -> [i32; 4] {
    let (wf, hf) = (width as f32, height as f32);
    [
        ceil(x0).clamp(0.0, wf) as i32,
        ceil(y0).clamp(0.0, hf) as i32,
        floor(x1).clamp(0.0, wf) as i32,
        floor(y1).clamp(0.0, hf) as i32,
    ]
}

fn intersect(a: [i32; 4], b: [i32; 4]) -> [i32; 4] {
    [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ]
}

/// `g`'s transform applied to `(x, y)` (`at` 0: to the screen; 6: from it).
fn apply(g: &GroupWords, at: usize, x: f32, y: f32) -> [f32; 2] {
    let m = |i: usize| f32::from_bits(g[at + i]);
    [m(0) * x + m(2) * y + m(4), m(1) * x + m(3) * y + m(5)]
}

/// The screen box of the local box `x0 y0 x1 y1` in group `g`.
fn screen_box(g: &GroupWords, x0: f32, y0: f32, x1: f32, y1: f32) -> [f32; 4] {
    let a = apply(g, 0, x0, y0);
    let b = apply(g, 0, x1, y0);
    let c = apply(g, 0, x0, y1);
    let d = apply(g, 0, x1, y1);
    [
        a[0].min(b[0]).min(c[0].min(d[0])),
        a[1].min(b[1]).min(c[1].min(d[1])),
        a[0].max(b[0]).max(c[0].max(d[0])),
        a[1].max(b[1]).max(c[1].max(d[1])),
    ]
}

/// The bounds of command `c` on a `width` × `height` surface.
pub fn bounds(c: &CommandWords, s: &Scene<'_>, width: u32, height: u32) -> Bounds {
    let p = &s.payload[c[3] as usize..];
    let g = &s.groups[group_of(c) as usize];
    let clip = &s.clips[clip_of(c) as usize];
    let cf = |i: usize| f32::from_bits(clip[i]);
    let clip_box = pixels(cf(10), cf(11), cf(12), cf(13), width, height);
    let none = [0; 4];
    let opaque = c[0] & OPAQUE != 0 && g[14] & GROUP_AXIS_ALIGNED != 0;
    // The local box (stroke and reach included), and the local box of full coverage.
    let (local, inner): ([f32; 4], Option<[f32; 4]>) = match Kind::from_word(c[0]) {
        Some(Kind::Rect) => {
            let e = w(p, 8) * 0.5;
            let inner = opaque.then(|| {
                let r = rect_radius(p).max4() + w(g, 13) * 0.5;
                [w(p, 0) + r, w(p, 1) + r, w(p, 2) - r, w(p, 3) - r]
            });
            ([w(p, 0) - e, w(p, 1) - e, w(p, 2) + e, w(p, 3) + e], inner)
        }
        Some(Kind::Circle) => {
            let (cx, cy, r) = (w(p, 0), w(p, 1), w(p, 2));
            let e = r + w(p, 3) * 0.5;
            let inner = opaque.then(|| {
                let h = (r - w(g, 13) * 0.5) * core::f32::consts::FRAC_1_SQRT_2;
                [cx - h, cy - h, cx + h, cy + h]
            });
            ([cx - e, cy - e, cx + e, cy + e], inner)
        }
        Some(Kind::Line) => {
            let e = line_reach(p);
            let (ax, ay, bx, by) = (w(p, 0), w(p, 1), w(p, 2), w(p, 3));
            (
                [
                    ax.min(bx) - e,
                    ay.min(by) - e,
                    ax.max(bx) + e,
                    ay.max(by) + e,
                ],
                None,
            )
        }
        Some(Kind::Triangle) => {
            let e = w(p, 6) * 0.5;
            let x0 = w(p, 0).min(w(p, 2)).min(w(p, 4));
            let y0 = w(p, 1).min(w(p, 3)).min(w(p, 5));
            let x1 = w(p, 0).max(w(p, 2)).max(w(p, 4));
            let y1 = w(p, 1).max(w(p, 3)).max(w(p, 5));
            ([x0 - e, y0 - e, x1 + e, y1 + e], None)
        }
        Some(Kind::Image) => ([w(p, 0), w(p, 1), w(p, 2), w(p, 3)], None),
        Some(Kind::Effect) => {
            // On the screen: the members' box, moved by the shadow's offset and grown by its
            // blur or the glow's radius.
            let (x0, y0, x1, y1) = (w(p, 2), w(p, 3), w(p, 4), w(p, 5));
            if x0 >= x1 {
                return Bounds::default();
            }
            let (dx, dy, e) = if p[0] == 0 {
                (w(g, 17), w(g, 18), w(g, 19) * 1.5)
            } else {
                (0.0, 0.0, w(g, 21))
            };
            let bbox = pixels(
                x0 + dx - e - 0.5,
                y0 + dy - e - 0.5,
                x1 + dx + e + 0.5,
                y1 + dy + e + 0.5,
                width,
                height,
            );
            return Bounds {
                bbox: intersect(bbox, clip_box),
                interior: none,
            };
        }
        Some(Kind::Glyph) => {
            // Glyphs are placed on the screen.
            let Some(gr) = s.glyphs.get(p[2] as usize) else {
                return Bounds::default();
            };
            let (gw, gh) = (gr[1] & 0xFFFF, gr[1] >> 16);
            let x0 = p[0] as i32 + (gr[2] & 0xFFFF) as u16 as i16 as i32;
            let y0 = p[1] as i32 + (gr[2] >> 16) as u16 as i16 as i32;
            let clampi = |v: i32, hi: u32| v.clamp(0, hi as i32);
            let bbox = [
                clampi(x0, width),
                clampi(y0, height),
                clampi(x0 + gw as i32, width),
                clampi(y0 + gh as i32, height),
            ];
            return Bounds {
                bbox: intersect(bbox, clip_box),
                interior: none,
            };
        }
        None => return Bounds::default(),
    };
    let sb = screen_box(g, local[0], local[1], local[2], local[3]);
    let bbox = intersect(
        pixels(
            sb[0] - 0.5,
            sb[1] - 0.5,
            sb[2] + 0.5,
            sb[3] + 0.5,
            width,
            height,
        ),
        clip_box,
    );
    let interior = match inner {
        Some(l) => {
            let si = screen_box(g, l[0], l[1], l[2], l[3]);
            intersect(
                inside(
                    si[0] - 0.5,
                    si[1] - 0.5,
                    si[2] + 0.5,
                    si[3] + 0.5,
                    width,
                    height,
                ),
                inside(cf(14), cf(15), cf(16), cf(17), width, height),
            )
        }
        None => none,
    };
    Bounds { bbox, interior }
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

/// Whether the 16 × 16 tile whose center is `(tx, ty)` may hold some of line command `c`'s ink
/// (§5.3, the conservative DDA): negative or zero when the tile's center is within the line's
/// reach plus the tile's, measured in the line's group.
pub fn line_touches_tile(c: &CommandWords, s: &Scene<'_>, tx: f32, ty: f32) -> f32 {
    let p = &s.payload[c[3] as usize..];
    let g = &s.groups[group_of(c) as usize];
    let t = apply(g, 6, tx, ty);
    let d = segment_distance(t, [w(p, 0), w(p, 1)], [w(p, 2), w(p, 3)]);
    d - (line_reach(p) + (TILE_RADIUS + 0.5) * w(g, 23))
}

#[derive(Clone, Copy)]
struct Radii([f32; 4]);

impl Radii {
    fn max4(self) -> f32 {
        self.0[0].max(self.0[1]).max(self.0[2].max(self.0[3]))
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

/// Distance to the rounded rectangle of `p[0..8]` (box, then radii).
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

/// Fill and outline coverage composed: `fill` color under the stroke. `d` and `stroke_width`
/// are in the group's units, `k` screen pixels per unit.
fn fill_and_stroke(fill: [f32; 4], d: f32, k: f32, stroke_width: f32, stroke: u32) -> [f32; 4] {
    let f = scale4(fill, coverage(d * k));
    if stroke_width <= 0.0 {
        return f;
    }
    let s = scale4(
        premultiplied(stroke),
        coverage((d.abs() - stroke_width * 0.5) * k),
    );
    over(s, f)
}

/// Screen pixels per unit of group `g` for the distance `dist` measures at screen pixel
/// `(x, y)`: the transform's scale, or for a non-uniform one the distance's gradient there.
fn pixels_per_unit(g: &GroupWords, x: f32, y: f32, d: f32, dist: &dyn Fn([f32; 2]) -> f32) -> f32 {
    if g[14] & GROUP_NONUNIFORM == 0 {
        return 1.0 / w(g, 13);
    }
    let dx = dist(apply(g, 6, x + 1.0, y)) - d;
    let dy = dist(apply(g, 6, x, y + 1.0)) - d;
    let grad = length(dx, dy);
    if grad > 1e-6 {
        1.0 / grad
    } else {
        1.0 / w(g, 13)
    }
}

/// The coverage of clip `clip`'s chain at screen pixel `(x, y)`.
pub fn clip_coverage(s: &Scene<'_>, clip: u32, x: f32, y: f32) -> f32 {
    let mut cov = 1.0;
    let mut c = clip;
    while c != 0 {
        let r = &s.clips[c as usize];
        let g = &s.groups[r[8] as usize];
        let dist = |q: [f32; 2]| rect_distance(r, q[0], q[1]);
        let q = apply(g, 6, x, y);
        let d = dist(q);
        cov *= coverage(d * pixels_per_unit(g, x, y, d, &dist));
        c = r[9];
    }
    cov
}

/// What the shader samples images and glyphs from.
pub trait Textures {
    /// The glyph page texel at (`x`, `y`) of `page`, 0–1.
    fn glyph(&self, page: u32, x: u32, y: u32) -> f32;
    /// Texture `view` through `sampler` at `uv`, mip `lod`.
    fn sample(&self, view: u32, sampler: u32, uv: [f32; 2], lod: f32) -> [f32; 4];
}

/// Command `c`'s premultiplied color (sRGB-encoded) at the pixel whose center is `(x, y)`.
pub fn shade(c: &CommandWords, s: &Scene<'_>, textures: &dyn Textures, x: f32, y: f32) -> [f32; 4] {
    let color = shade_unclipped(c, s, textures, x, y);
    let g = &s.groups[group_of(c) as usize];
    let k = w(g, 12) * clip_coverage(s, clip_of(c), x, y);
    scale4(color, k)
}

fn shade_unclipped(
    c: &CommandWords,
    s: &Scene<'_>,
    textures: &dyn Textures,
    x: f32,
    y: f32,
) -> [f32; 4] {
    let p = &s.payload[c[3] as usize..];
    let paint = premultiplied(s.paints[c[2] as usize][1]);
    let g = &s.groups[group_of(c) as usize];
    let q = apply(g, 6, x, y);
    // A distance at `q` and the pixels per unit there.
    let measure = |dist: &dyn Fn([f32; 2]) -> f32| {
        let d = dist(q);
        (d, pixels_per_unit(g, x, y, d, dist))
    };
    match Kind::from_word(c[0]) {
        Some(Kind::Effect) => effect(c, s, paint, x, y),
        Some(Kind::Rect) => {
            let (d, k) = measure(&|q| rect_distance(p, q[0], q[1]));
            fill_and_stroke(paint, d, k, w(p, 8), p[9])
        }
        Some(Kind::Circle) => {
            let (d, k) = measure(&|q| length(q[0] - w(p, 0), q[1] - w(p, 1)) - w(p, 2));
            fill_and_stroke(paint, d, k, w(p, 3), p[4])
        }
        Some(Kind::Line) => {
            let (d, k) = measure(&|q| line_distance(p, q[0], q[1]));
            scale4(paint, coverage(d * k))
        }
        Some(Kind::Triangle) => {
            let (d, k) = measure(&|q| {
                triangle_distance(
                    q,
                    [w(p, 0), w(p, 1)],
                    [w(p, 2), w(p, 3)],
                    [w(p, 4), w(p, 5)],
                )
            });
            fill_and_stroke(paint, d, k, w(p, 6), p[7])
        }
        Some(Kind::Glyph) => {
            let Some(gr) = s.glyphs.get(p[2] as usize) else {
                return [0.0; 4];
            };
            let (gw, gh) = ((gr[1] & 0xFFFF) as i32, (gr[1] >> 16) as i32);
            let gx = floor(x) as i32 - (p[0] as i32 + (gr[2] & 0xFFFF) as u16 as i16 as i32);
            let gy = floor(y) as i32 - (p[1] as i32 + (gr[2] >> 16) as u16 as i16 as i32);
            if gx < 0 || gy < 0 || gx >= gw || gy >= gh {
                return [0.0; 4];
            }
            let t = textures.glyph(
                gr[3],
                (gr[0] & 0xFFFF) + gx as u32,
                (gr[0] >> 16) + gy as u32,
            );
            scale4(paint, t)
        }
        Some(Kind::Image) => {
            // The radii follow the texture size; the distance clamps them.
            let rect = [p[0], p[1], p[2], p[3], p[9], p[10], p[11], p[12]];
            let (d, k) = measure(&|q| rect_distance(&rect, q[0], q[1]));
            let cov = coverage(d * k);
            if cov <= 0.0 {
                return [0.0; 4];
            }
            let (x0, y0, x1, y1) = (w(p, 0), w(p, 1), w(p, 2), w(p, 3));
            let (u0, v0, u1, v1) = (w(p, 4), w(p, 5), w(p, 6), w(p, 7));
            let du = (u1 - u0) / (x1 - x0);
            let dv = (v1 - v0) / (y1 - y0);
            let uv = [u0 + (q[0] - x0) * du, v0 + (q[1] - y0) * dv];
            let (tw, th) = ((p[8] & 0xFFFF) as f32, (p[8] >> 16) as f32);
            let texels = (du.abs() * tw).max(dv.abs() * th) * w(g, 13);
            let lod = marlin_math::scalar::log2(texels.max(1e-6)).max(0.0);
            let pw = &s.paints[c[2] as usize];
            let mut t = textures.sample(pw[2], pw[3], uv, lod);
            if c[0] & IMAGE_SRGB_PREMULTIPLIED == 0 {
                let a = t[3];
                t = [
                    linear_to_srgb(t[0]) * a,
                    linear_to_srgb(t[1]) * a,
                    linear_to_srgb(t[2]) * a,
                    a,
                ];
            }
            // Both premultiplied: the texel's color times the tint's.
            let tinted = [
                t[0] * paint[0],
                t[1] * paint[1],
                t[2] * paint[2],
                t[3] * paint[3],
            ];
            scale4(tinted, cov)
        }
        None => [0.0; 4],
    }
}

/// `erf(x)`, Abramowitz and Stegun 7.1.26 (error below 1.5e-7).
pub fn erf(x: f32) -> f32 {
    let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
    let y = 1.0
        - (((((1.061_405_4 * t - 1.453_152) * t) + 1.421_413_8) * t - 0.284_496_74) * t
            + 0.254_829_6)
            * t
            * marlin_math::scalar::exp(-x * x);
    if x < 0.0 { -y } else { y }
}

/// A member's distance at screen pixel `(x, y)`, in screen pixels: its shape's outer edge
/// (strokes included), a glyph's bitmap box; nothing for effects.
fn member_distance(c: &CommandWords, s: &Scene<'_>, x: f32, y: f32) -> f32 {
    let p = &s.payload[c[3] as usize..];
    let g = &s.groups[group_of(c) as usize];
    let q = apply(g, 6, x, y);
    let k = 1.0 / w(g, 13);
    let d = match Kind::from_word(c[0]) {
        Some(Kind::Rect) => rect_distance(p, q[0], q[1]) - w(p, 8) * 0.5,
        Some(Kind::Circle) => length(q[0] - w(p, 0), q[1] - w(p, 1)) - w(p, 2) - w(p, 3) * 0.5,
        Some(Kind::Line) => line_distance(p, q[0], q[1]),
        Some(Kind::Triangle) => {
            triangle_distance(
                q,
                [w(p, 0), w(p, 1)],
                [w(p, 2), w(p, 3)],
                [w(p, 4), w(p, 5)],
            ) - w(p, 6) * 0.5
        }
        Some(Kind::Image) => {
            let rect = [p[0], p[1], p[2], p[3], p[9], p[10], p[11], p[12]];
            rect_distance(&rect, q[0], q[1])
        }
        Some(Kind::Glyph) => {
            let Some(gr) = s.glyphs.get(p[2] as usize) else {
                return f32::MAX;
            };
            let x0 = (p[0] as i32 + (gr[2] & 0xFFFF) as u16 as i16 as i32) as f32;
            let y0 = (p[1] as i32 + (gr[2] >> 16) as u16 as i16 as i32) as f32;
            let (gw, gh) = ((gr[1] & 0xFFFF) as f32, (gr[1] >> 16) as f32);
            if gw == 0.0 {
                return f32::MAX;
            }
            return round_box(
                x - (x0 + gw * 0.5),
                y - (y0 + gh * 0.5),
                gw * 0.5,
                gh * 0.5,
                0.0,
            );
        }
        Some(Kind::Effect) | None => return f32::MAX,
    };
    d * k
}

/// The distance to a group effect's members at screen pixel `(x, y)`: the nearest member, or
/// their box as a rounded rectangle.
fn effect_distance(c: &CommandWords, s: &Scene<'_>, x: f32, y: f32) -> f32 {
    let p = &s.payload[c[3] as usize..];
    let g = &s.groups[group_of(c) as usize];
    let (first, count) = (g[15], g[16]);
    let radius = w(p, 6);
    if radius >= 0.0 || count > crate::list::MAX_EFFECT_MEMBERS {
        let (x0, y0, x1, y1) = (w(p, 2), w(p, 3), w(p, 4), w(p, 5));
        let r = radius.max(0.0).min((x1 - x0).min(y1 - y0) * 0.5);
        return round_box(
            x - (x0 + x1) * 0.5,
            y - (y0 + y1) * 0.5,
            (x1 - x0) * 0.5,
            (y1 - y0) * 0.5,
            r,
        );
    }
    let mut d = f32::MAX;
    for i in first..first + count {
        d = d.min(member_distance(&s.commands[i as usize], s, x, y));
    }
    d
}

/// A group's shadow (a Gaussian blur of the members' shape: half the blur radius is σ) or
/// glow (fading quadratically over its radius), §4.3.
fn effect(c: &CommandWords, s: &Scene<'_>, paint: [f32; 4], x: f32, y: f32) -> [f32; 4] {
    let p = &s.payload[c[3] as usize..];
    let g = &s.groups[group_of(c) as usize];
    let a = if p[0] == 0 {
        let d = effect_distance(c, s, x - w(g, 17), y - w(g, 18));
        let sigma = w(g, 19) * 0.5;
        if sigma > 0.0 {
            0.5 - 0.5 * erf(d / (sigma * core::f32::consts::SQRT_2))
        } else {
            coverage(d)
        }
    } else {
        let d = effect_distance(c, s, x, y);
        let r = w(g, 21);
        if r > 0.0 {
            let t = 1.0 - (d / r).clamp(0.0, 1.0);
            t * t
        } else {
            coverage(d)
        }
    };
    scale4(paint, a)
}
