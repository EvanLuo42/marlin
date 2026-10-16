//! BC1 color blocks, also the color half of BC3.
//!
//! Two RGB565 endpoints and 2-bit indices. `c0 > c1` selects four colors (the endpoints and the
//! 1/3, 2/3 points); otherwise three (the midpoint, and index 3 is transparent black). BC3's
//! color block always decodes four colors.

use std::sync::OnceLock;

use super::fit::{self, Vec4};
use crate::format::{Interpolation, Quality};

pub(crate) struct Params {
    pub quality: Quality,
    pub weights: [f32; 4],
    /// BC3: four colors whatever the endpoint order.
    pub four_only: bool,
    /// Texels with alpha (0–255) below this become transparent. `None`: opaque.
    pub alpha_cutoff: Option<f32>,
    pub mode_mask: u32,
}

const fn expand5(v: u32) -> u32 {
    (v << 3) | (v >> 2)
}

const fn expand6(v: u32) -> u32 {
    (v << 2) | (v >> 4)
}

fn endpoint(c: u16) -> [f32; 3] {
    let c = c as u32;
    [
        expand5(c >> 11) as f32,
        expand6((c >> 5) & 63) as f32,
        expand5(c & 31) as f32,
    ]
}

/// The palette, 0–255. `four`: the four-color interpretation.
fn palette(c0: u16, c1: u16, four: bool) -> [Vec4; 4] {
    let a = endpoint(c0);
    let b = endpoint(c1);
    let mix = |wa: f32, wb: f32| -> Vec4 {
        let d = wa + wb;
        [
            (wa * a[0] + wb * b[0]) / d,
            (wa * a[1] + wb * b[1]) / d,
            (wa * a[2] + wb * b[2]) / d,
            255.0,
        ]
    };
    let a4 = [a[0], a[1], a[2], 255.0];
    let b4 = [b[0], b[1], b[2], 255.0];
    if four {
        [a4, b4, mix(2.0, 1.0), mix(1.0, 2.0)]
    } else {
        [a4, b4, mix(1.0, 1.0), [0.0; 4]]
    }
}

/// NVIDIA's palette: 8-bit results of fixed-point interpolation, red and blue from the 5-bit
/// values, green from the expanded 8-bit ones.
fn palette_nvidia(c0: u16, c1: u16, four: bool) -> [Vec4; 4] {
    let (c0, c1) = (c0 as i32, c1 as i32);
    let (r0, g0, b0) = (c0 >> 11, expand6(((c0 >> 5) & 63) as u32) as i32, c0 & 31);
    let (r1, g1, b1) = (c1 >> 11, expand6(((c1 >> 5) & 63) as u32) as i32, c1 & 31);
    // Truncating division, as the hardware's arithmetic.
    let gdiff = g1 - g0;
    let rgb = |r: i32, g: i32, b: i32| [r as f32, g as f32, b as f32, 255.0];
    let p0 = rgb(r0 * 66 / 8, g0, b0 * 66 / 8);
    let p1 = rgb(r1 * 66 / 8, g1, b1 * 66 / 8);
    if four {
        [
            p0,
            p1,
            rgb(
                (2 * r0 + r1) * 22 / 8,
                (256 * g0 + gdiff / 4 + 128 + gdiff * 80) / 256,
                (2 * b0 + b1) * 22 / 8,
            ),
            rgb(
                (2 * r1 + r0) * 22 / 8,
                (256 * g1 - gdiff / 4 + 128 - gdiff * 80) / 256,
                (2 * b1 + b0) * 22 / 8,
            ),
        ]
    } else {
        [
            p0,
            p1,
            rgb(
                (r0 + r1) * 33 / 8,
                (256 * g0 + gdiff / 4 + 128 + gdiff * 128) / 256,
                (b0 + b1) * 33 / 8,
            ),
            [0.0; 4],
        ]
    }
}

/// Texels as RGBA in [0, 1].
pub(crate) fn decode(
    block: &[u8],
    four_only: bool,
    interpolation: Interpolation,
) -> [[f32; 4]; 16] {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    let four = four_only || c0 > c1;
    let palette = match interpolation {
        Interpolation::Exact => palette(c0, c1, four),
        Interpolation::Nvidia => palette_nvidia(c0, c1, four),
    };
    core::array::from_fn(|i| palette[(indices >> (2 * i)) as usize & 3].map(|c| c / 255.0))
}

pub(crate) fn encode(texels: &[Vec4; 16], params: &Params) -> [u8; 8] {
    let transparent: [bool; 16] =
        core::array::from_fn(|i| params.alpha_cutoff.is_some_and(|c| texels[i][3] < c));
    let mut points = [[0.0; 4]; 16];
    let mut n = 0;
    for (t, &skip) in texels.iter().zip(&transparent) {
        if !skip {
            points[n] = *t;
            n += 1;
        }
    }
    if n == 0 {
        // Equal endpoints select three colors; index 3 everywhere is transparent black.
        return pack(0, 0, u32::MAX);
    }
    let points = &points[..n];
    let ctx = Ctx {
        texels,
        transparent,
        weights: params.weights,
        four_only: params.four_only,
    };

    // Transparent texels need the three-color mode, which BC3 does not have.
    let any_transparent = n < 16 && !params.four_only;
    let mut modes = Vec::with_capacity(2);
    if !any_transparent && params.mode_mask & 1 != 0 {
        modes.push(true);
    }
    if !params.four_only && (any_transparent || params.mode_mask & 2 != 0) {
        modes.push(false);
    }
    if modes.is_empty() {
        modes.push(true);
    }

    let mut best = Candidate {
        c0: 0,
        c1: 0,
        indices: 0,
        error: f32::MAX,
    };
    let (lo, hi) = fit::axis_endpoints(points, 0, 3);
    let mean = fit::mean(points, 0, 3);
    for &four in &modes {
        let mut consider = |a: [f32; 3], b: [f32; 3]| {
            let (c0, c1) = ctx.order(quantize(a), quantize(b), four);
            let candidate = ctx.evaluate(c0, c1);
            if candidate.error < best.error {
                best = candidate;
            }
            candidate
        };

        // Principal axis extremes, then least-squares refits of the endpoints to the indices.
        let mut current = consider(rgb(hi), rgb(lo));
        let iterations = match params.quality {
            Quality::Fast => 1,
            Quality::Normal => 3,
            Quality::High => 4,
        };
        for _ in 0..iterations {
            let alphas = ctx.alphas(current, points);
            let Some((a, b)) = fit::least_squares(points, &alphas[..n], 0, 3) else {
                break;
            };
            let next = consider(rgb(a), rgb(b));
            if next.error >= current.error {
                break;
            }
            current = next;
        }

        if params.quality >= Quality::Normal {
            // The single color closest to the mean, through the interpolated point.
            let (a, b) = solid(rgb(mean), four);
            consider(a, b);
        }
        if params.quality >= Quality::High
            && let Some((a, b)) = cluster_fit(points, four)
        {
            consider(a, b);
        }
    }

    if params.quality >= Quality::High {
        best = ctx.refine(best);
    }
    pack(best.c0, best.c1, best.indices)
}

fn rgb(v: Vec4) -> [f32; 3] {
    [v[0], v[1], v[2]]
}

fn pack(c0: u16, c1: u16, indices: u32) -> [u8; 8] {
    let mut out = [0; 8];
    out[..2].copy_from_slice(&c0.to_le_bytes());
    out[2..4].copy_from_slice(&c1.to_le_bytes());
    out[4..].copy_from_slice(&indices.to_le_bytes());
    out
}

/// RGB (0–255) to the nearest RGB565.
fn quantize(c: [f32; 3]) -> u16 {
    let r = nearest(c[0], 5);
    let g = nearest(c[1], 6);
    let b = nearest(c[2], 5);
    ((r << 11) | (g << 5) | b) as u16
}

/// The `bits`-bit value whose bit-replicated expansion is closest to `v` (0–255).
fn nearest(v: f32, bits: u32) -> u32 {
    let max: i32 = (1 << bits) - 1;
    let expand = |q: u32| if bits == 5 { expand5(q) } else { expand6(q) } as f32;
    let guess = (v.clamp(0.0, 255.0) * max as f32 / 255.0).round() as i32;
    (guess - 1..=guess + 1)
        .filter(|&q| (0..=max).contains(&q))
        .min_by(|&a, &b| {
            (expand(a as u32) - v)
                .abs()
                .total_cmp(&(expand(b as u32) - v).abs())
        })
        .unwrap() as u32
}

#[derive(Clone, Copy)]
struct Candidate {
    c0: u16,
    c1: u16,
    indices: u32,
    error: f32,
}

struct Ctx<'a> {
    texels: &'a [Vec4; 16],
    transparent: [bool; 16],
    weights: [f32; 4],
    four_only: bool,
}

impl Ctx<'_> {
    /// Endpoint order that selects the wanted mode.
    fn order(&self, a: u16, b: u16, four: bool) -> (u16, u16) {
        if self.four_only {
            (a, b)
        } else if four {
            (a.max(b), a.min(b))
        } else {
            (a.min(b), a.max(b))
        }
    }

    fn evaluate(&self, c0: u16, c1: u16) -> Candidate {
        let four = self.four_only || c0 > c1;
        let palette = palette(c0, c1, four);
        let usable = if four { 4 } else { 3 };
        let w = self.weights;
        let mut indices = 0;
        let mut error = 0.0;
        for (i, t) in self.texels.iter().enumerate() {
            if self.transparent[i] {
                indices |= 3 << (2 * i);
                continue;
            }
            let mut best = (0, f32::MAX);
            for (k, p) in palette[..usable].iter().enumerate() {
                let d: f32 = (0..3).map(|c| w[c] * (p[c] - t[c]) * (p[c] - t[c])).sum();
                if d < best.1 {
                    best = (k, d);
                }
            }
            indices |= (best.0 as u32) << (2 * i);
            error += best.1;
        }
        Candidate {
            c0,
            c1,
            indices,
            error,
        }
    }

    /// Each opaque texel's interpolation weight on `c0`, in `points` order.
    fn alphas(&self, candidate: Candidate, points: &[Vec4]) -> [f32; 16] {
        let four = self.four_only || candidate.c0 > candidate.c1;
        let table: [f32; 4] = if four {
            [1.0, 0.0, 2.0 / 3.0, 1.0 / 3.0]
        } else {
            [1.0, 0.0, 0.5, 0.0]
        };
        let mut alphas = [0.0; 16];
        let mut n = 0;
        for i in 0..16 {
            if !self.transparent[i] {
                alphas[n] = table[(candidate.indices >> (2 * i)) as usize & 3];
                n += 1;
            }
        }
        debug_assert_eq!(n, points.len());
        alphas
    }

    /// Coordinate descent over the six 565 components, keeping the mode.
    fn refine(&self, mut best: Candidate) -> Candidate {
        let four = self.four_only || best.c0 > best.c1;
        // (shift, mask) of the R, G and B fields.
        const FIELDS: [(u32, u32); 3] = [(11, 31), (5, 63), (0, 31)];
        for _ in 0..8 {
            let mut improved = false;
            for end in 0..2 {
                for (shift, mask) in FIELDS {
                    for step in [-1i32, 1] {
                        let mut ends = [best.c0, best.c1];
                        let field = (ends[end] as u32 >> shift) & mask;
                        let Some(v) = field.checked_add_signed(step).filter(|&v| v <= mask) else {
                            continue;
                        };
                        ends[end] = ((ends[end] as u32 & !(mask << shift)) | (v << shift)) as u16;
                        let (c0, c1) = self.order(ends[0], ends[1], four);
                        let candidate = self.evaluate(c0, c1);
                        if candidate.error < best.error {
                            best = candidate;
                            improved = true;
                        }
                    }
                }
            }
            if !improved {
                break;
            }
        }
        best
    }
}

/// Endpoints whose interpolated color (index 2) best matches `color`, per channel.
fn solid(color: [f32; 3], four: bool) -> ([f32; 3], [f32; 3]) {
    let tables = solid_tables();
    let mut a = [0.0; 3];
    let mut b = [0.0; 3];
    for c in 0..3 {
        let six = c == 1;
        let table = &tables[four as usize * 2 + six as usize];
        let [qa, qb] = table[color[c].round().clamp(0.0, 255.0) as usize];
        let expand = |q: u8| if six { expand6(q as u32) } else { expand5(q as u32) } as f32;
        a[c] = expand(qa);
        b[c] = expand(qb);
    }
    (a, b)
}

/// For each 8-bit value, the quantized endpoint pair whose index-2 color is closest: [three-color
/// 5-bit, three-color 6-bit, four-color 5-bit, four-color 6-bit]. Ties go to the closest pair,
/// which also keeps hardware interpolation error small.
fn solid_tables() -> &'static [[[u8; 2]; 256]; 4] {
    static TABLES: OnceLock<[[[u8; 2]; 256]; 4]> = OnceLock::new();
    TABLES.get_or_init(|| {
        core::array::from_fn(|t| {
            let four = t >= 2;
            let bits = if t % 2 == 1 { 6 } else { 5 };
            let expand = |q: u32| if bits == 6 { expand6(q) } else { expand5(q) } as f32;
            core::array::from_fn(|v| {
                let mut best = ([0u8; 2], f32::MAX, u32::MAX);
                for a in 0..1u32 << bits {
                    for b in 0..1u32 << bits {
                        let mid = if four {
                            (2.0 * expand(a) + expand(b)) / 3.0
                        } else {
                            (expand(a) + expand(b)) / 2.0
                        };
                        let error = (mid - v as f32).abs();
                        let spread = a.abs_diff(b);
                        if error < best.1 - 1e-4 || (error <= best.1 + 1e-4 && spread < best.2) {
                            best = ([a as u8, b as u8], error, spread);
                        }
                    }
                }
                best.0
            })
        })
    })
}

/// Cluster fit: over every split of the texels, ordered along the principal axis, into runs
/// that share an index, the least-squares endpoints with the least error (unquantized).
fn cluster_fit(points: &[Vec4], four: bool) -> Option<([f32; 3], [f32; 3])> {
    let n = points.len();
    let mean = fit::mean(points, 0, 3);
    let axis = fit::principal_axis(points, mean, 0, 3);
    let mut order: Vec<usize> = (0..n).collect();
    let t = |p: &Vec4| (0..3).map(|c| (p[c] - mean[c]) * axis[c]).sum::<f32>();
    order.sort_by(|&a, &b| t(&points[b]).total_cmp(&t(&points[a])));
    let mut prefix = vec![[0.0f32; 3]; n + 1];
    for (k, &i) in order.iter().enumerate() {
        for c in 0..3 {
            prefix[k + 1][c] = prefix[k][c] + points[i][c];
        }
    }
    let range =
        |a: usize, b: usize| -> [f32; 3] { core::array::from_fn(|c| prefix[b][c] - prefix[a][c]) };

    // Clusters take weights on `a` of [1, 2/3, 1/3, 0] (four) or [1, 1/2, 0] (three).
    let mut best: Option<(f32, [f32; 3], [f32; 3])> = None;
    let mut solve = |counts: [f32; 4], sums: [[f32; 3]; 4], alphas: [f32; 4]| {
        let (mut aa, mut ab, mut bb) = (0.0, 0.0, 0.0);
        let mut ax = [0.0f32; 3];
        let mut bx = [0.0f32; 3];
        for k in 0..4 {
            let (wa, wb) = (alphas[k], 1.0 - alphas[k]);
            aa += counts[k] * wa * wa;
            ab += counts[k] * wa * wb;
            bb += counts[k] * wb * wb;
            for c in 0..3 {
                ax[c] += wa * sums[k][c];
                bx[c] += wb * sums[k][c];
            }
        }
        let det = aa * bb - ab * ab;
        if det.abs() < 1e-5 {
            return;
        }
        let inv = 1.0 / det;
        let a: [f32; 3] = core::array::from_fn(|c| (ax[c] * bb - bx[c] * ab) * inv);
        let b: [f32; 3] = core::array::from_fn(|c| (bx[c] * aa - ax[c] * ab) * inv);
        // Σ|x|² is common to every split, so it is left out.
        let error: f32 = (0..3)
            .map(|c| {
                aa * a[c] * a[c] + bb * b[c] * b[c] + 2.0 * ab * a[c] * b[c]
                    - 2.0 * (a[c] * ax[c] + b[c] * bx[c])
            })
            .sum();
        if best.is_none_or(|(e, ..)| error < e) {
            best = Some((error, a, b));
        }
    };
    if four {
        for i in 0..=n {
            for j in i..=n {
                for k in j..=n {
                    let counts = [i, j - i, k - j, n - k].map(|c| c as f32);
                    let sums = [range(0, i), range(i, j), range(j, k), range(k, n)];
                    solve(counts, sums, [1.0, 2.0 / 3.0, 1.0 / 3.0, 0.0]);
                }
            }
        }
    } else {
        for i in 0..=n {
            for j in i..=n {
                let counts = [i, j - i, n - j, 0].map(|c| c as f32);
                let sums = [range(0, i), range(i, j), range(j, n), [0.0; 3]];
                solve(counts, sums, [1.0, 0.5, 0.0, 0.0]);
            }
        }
    }
    best.map(|(_, a, b)| (a, b))
}
