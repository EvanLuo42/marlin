//! BC7: eight modes trading subsets (partitions), endpoint precision, index precision and
//! separate alpha.
//!
//! The encoder fits each subset's endpoints on the principal axis, then alternates index
//! assignment with least-squares refits; higher qualities search more modes, partitions,
//! rotations and quantized-endpoint neighbors. Partitions are ranked by how well a line fits
//! each subset before any full fit.

use super::bits::{BitReader, BitWriter};
use super::fit::{self, Vec4};
use super::tables;
use crate::format::Quality;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PBits {
    None,
    /// One per endpoint.
    Unique,
    /// One per subset, shared by its endpoints.
    Shared,
}

#[derive(Clone, Copy)]
struct Mode {
    subsets: usize,
    partition_bits: u32,
    rotation_bits: u32,
    index_selection_bits: u32,
    color_bits: u32,
    alpha_bits: u32,
    pbits: PBits,
    index_bits: u32,
    index2_bits: u32,
}

/// The D3D11 spec's mode table, one row per mode.
const MODES: [Mode; 8] = {
    const fn row(fields: [u32; 8], pbits: PBits) -> Mode {
        let [
            subsets,
            partition_bits,
            rotation_bits,
            index_selection_bits,
            color_bits,
            alpha_bits,
            index_bits,
            index2_bits,
        ] = fields;
        Mode {
            subsets: subsets as usize,
            partition_bits,
            rotation_bits,
            index_selection_bits,
            color_bits,
            alpha_bits,
            pbits,
            index_bits,
            index2_bits,
        }
    }
    // Subsets, partition bits, rotation bits, index selection bits, color bits, alpha bits,
    // index bits, secondary index bits.
    [
        row([3, 4, 0, 0, 4, 0, 3, 0], PBits::Unique),
        row([2, 6, 0, 0, 6, 0, 3, 0], PBits::Shared),
        row([3, 6, 0, 0, 5, 0, 2, 0], PBits::None),
        row([2, 6, 0, 0, 7, 0, 2, 0], PBits::Unique),
        row([1, 0, 2, 1, 5, 6, 2, 3], PBits::None),
        row([1, 0, 2, 0, 7, 8, 2, 2], PBits::None),
        row([1, 0, 0, 0, 7, 7, 4, 0], PBits::Unique),
        row([2, 6, 0, 0, 5, 5, 2, 0], PBits::Unique),
    ]
};

const WEIGHTS2: [u32; 4] = [0, 21, 43, 64];
const WEIGHTS3: [u32; 8] = [0, 9, 18, 27, 37, 46, 55, 64];
const WEIGHTS4: [u32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

pub(crate) fn weights(bits: u32) -> &'static [u32] {
    match bits {
        2 => &WEIGHTS2,
        3 => &WEIGHTS3,
        _ => &WEIGHTS4,
    }
}

pub(crate) fn interpolate(e0: u32, e1: u32, w: u32) -> u32 {
    ((64 - w) * e0 + w * e1 + 32) >> 6
}

/// [`interpolate`] for BC6H's signed 16-bit endpoints (the shift floors).
pub(crate) fn interpolate_signed(e0: i32, e1: i32, w: u32) -> i32 {
    ((64 - w as i32) * e0 + w as i32 * e1 + 32) >> 6
}

/// A `bits`-bit endpoint value widened to 8 bits by replicating its top bits.
fn expand(v: u32, bits: u32) -> u32 {
    if bits >= 8 {
        v
    } else {
        (v << (8 - bits)) | (v >> (2 * bits - 8))
    }
}

/// The mode of an encoded block; `None` for the reserved all-zero mode byte.
pub(crate) fn block_mode(block: &[u8; 16]) -> Option<usize> {
    (block[0] != 0).then(|| block[0].trailing_zeros() as usize)
}

/// Texels as 8-bit RGBA.
pub(crate) fn decode(block: &[u8; 16]) -> [[u8; 4]; 16] {
    let Some(mode_index) = block_mode(block) else {
        // Reserved: the spec decodes it to transparent black.
        return [[0; 4]; 16];
    };
    let mode = MODES[mode_index];
    let mut r = BitReader::new(block);
    r.read(mode_index as u32 + 1);
    let partition = r.read(mode.partition_bits) as usize;
    let rotation = r.read(mode.rotation_bits);
    let index_selection = r.read(mode.index_selection_bits);

    let mut ends = [[[0u32; 4]; 2]; 3];
    let channels = if mode.alpha_bits > 0 { 4 } else { 3 };
    for c in 0..channels {
        let bits = if c == 3 {
            mode.alpha_bits
        } else {
            mode.color_bits
        };
        for subset in ends.iter_mut().take(mode.subsets) {
            for end in subset.iter_mut() {
                end[c] = r.read(bits);
            }
        }
    }
    let pbit = mode.pbits != PBits::None;
    for subset in ends.iter_mut().take(mode.subsets) {
        let p = match mode.pbits {
            PBits::None => [0, 0],
            PBits::Unique => [r.read(1), r.read(1)],
            PBits::Shared => {
                let p = r.read(1);
                [p, p]
            }
        };
        for (end, p) in subset.iter_mut().zip(p) {
            for (c, v) in end.iter_mut().enumerate().take(channels) {
                let bits = if c == 3 {
                    mode.alpha_bits
                } else {
                    mode.color_bits
                };
                let q = if pbit { (*v << 1) | p } else { *v };
                *v = expand(q, bits + pbit as u32);
            }
            if channels == 3 {
                end[3] = 255;
            }
        }
    }

    let mut indices = [0u32; 16];
    for (i, index) in indices.iter_mut().enumerate() {
        let anchor = tables::is_anchor(mode.subsets, partition, i);
        *index = r.read(mode.index_bits - anchor as u32);
    }
    let mut indices2 = [0u32; 16];
    if mode.index2_bits > 0 {
        for (i, index) in indices2.iter_mut().enumerate() {
            *index = r.read(mode.index2_bits - (i == 0) as u32);
        }
    }

    core::array::from_fn(|i| {
        let s = tables::subset(mode.subsets, partition, i);
        let [e0, e1] = ends[s];
        let (color_index, color_bits, alpha_index, alpha_bits) = if mode.index2_bits == 0 {
            (indices[i], mode.index_bits, indices[i], mode.index_bits)
        } else if index_selection == 0 {
            (indices[i], mode.index_bits, indices2[i], mode.index2_bits)
        } else {
            (indices2[i], mode.index2_bits, indices[i], mode.index_bits)
        };
        let cw = weights(color_bits)[color_index as usize];
        let aw = weights(alpha_bits)[alpha_index as usize];
        let mut t = [
            interpolate(e0[0], e1[0], cw) as u8,
            interpolate(e0[1], e1[1], cw) as u8,
            interpolate(e0[2], e1[2], cw) as u8,
            interpolate(e0[3], e1[3], aw) as u8,
        ];
        if rotation > 0 {
            t.swap(rotation as usize - 1, 3);
        }
        t
    })
}

pub(crate) struct Params {
    pub quality: Quality,
    pub weights: [f32; 4],
    pub mode_mask: u32,
}

/// Search effort. Every candidate (mode, partition, rotation, index selection) gets one quick
/// fit; the best few are then refitted thoroughly.
struct Effort {
    /// Partitions tried per two- and three-subset mode, best line fit first.
    partitions2: usize,
    partitions3: usize,
    /// All four channel rotations of modes 4 and 5, and both index selections of mode 4.
    rotations: bool,
    /// Candidates refitted after the quick pass, and how.
    finalists: usize,
    final_pass: Pass,
}

/// How thoroughly one candidate is fitted.
#[derive(Clone, Copy)]
struct Pass {
    /// Rounds of index assignment and least-squares endpoint refits.
    iterations: u32,
    /// Coordinate descent over the quantized endpoints and p-bits.
    refine: bool,
    /// Every p-bit combination, not just the one that quantizes the endpoints best.
    all_pbits: bool,
}

const QUICK: Pass = Pass {
    iterations: 1,
    refine: false,
    all_pbits: false,
};

/// One subset's fitted endpoints.
#[derive(Clone, Copy, Default)]
struct Ends {
    /// Quantized values, without p-bits.
    q: [[u32; 4]; 2],
    p: [u32; 2],
}

/// Everything a block encodes.
#[derive(Clone, Copy)]
struct Encoding {
    mode: usize,
    partition: usize,
    rotation: u32,
    index_selection: u32,
    ends: [Ends; 3],
    /// Mode 4 and 5: the separately fitted alpha endpoints (subset 0 only).
    alpha: Ends,
    indices: [u32; 16],
    /// Mode 4 and 5: alpha's indices.
    alpha_indices: [u32; 16],
    error: f32,
}

/// A point of the search space.
#[derive(Clone, Copy)]
struct Candidate {
    mode: usize,
    partition: usize,
    rotation: usize,
    selection: u32,
}

/// Texels 0–255.
pub(crate) fn encode(texels: &[Vec4; 16], params: &Params) -> [u8; 16] {
    let effort = match params.quality {
        Quality::Fast => Effort {
            partitions2: 1,
            partitions3: 1,
            rotations: false,
            finalists: 1,
            final_pass: Pass {
                iterations: 2,
                refine: false,
                all_pbits: true,
            },
        },
        Quality::Normal => Effort {
            partitions2: 6,
            partitions3: 3,
            rotations: true,
            finalists: 2,
            final_pass: Pass {
                iterations: 3,
                refine: false,
                all_pbits: true,
            },
        },
        Quality::High => Effort {
            partitions2: 24,
            partitions3: 12,
            rotations: true,
            finalists: 4,
            final_pass: Pass {
                iterations: 4,
                refine: true,
                all_pbits: true,
            },
        },
    };
    let opaque = texels.iter().all(|t| t[3] >= 254.5);
    let default_modes: u32 = match (params.quality, opaque) {
        (Quality::Fast, true) => 1 << 6 | 1 << 1 | 1 << 3,
        (Quality::Fast, false) => 1 << 6 | 1 << 5 | 1 << 7,
        // Modes 0 to 3 have no alpha.
        (_, false) => 0b1111_0000,
        (_, true) => 0xff,
    };
    let mut modes = default_modes & params.mode_mask;
    if modes == 0 {
        modes = params.mode_mask & 0xff;
    }
    assert!(modes != 0, "BC7 mode mask allows no mode");

    let ctx = Ctx {
        texels,
        weights: params.weights,
    };
    // Rankings shared by the modes with the same subsets and channels: two subsets RGB (modes 1
    // and 3) and RGBA (7), three subsets RGB (0 and 2).
    let mut rankings: [Option<Vec<usize>>; 3] = [None, None, None];
    let mut candidates = Vec::new();
    for m in (0..8).filter(|m| modes & (1 << m) != 0) {
        let mode = MODES[m];
        if mode.index2_bits > 0 {
            // Opaque blocks rarely gain from rotating a color channel into the scalar slot.
            let rotate = effort.rotations && (!opaque || params.quality == Quality::High);
            let rotations = if rotate { 4 } else { 1 };
            let selections = if m == 4 && effort.rotations { 2 } else { 1 };
            for rotation in 0..rotations {
                for selection in 0..selections {
                    candidates.push(Candidate {
                        mode: m,
                        partition: 0,
                        rotation,
                        selection,
                    });
                }
            }
            continue;
        }
        let (slot, keep) = match (mode.subsets, mode.alpha_bits > 0) {
            (1, _) => {
                candidates.push(Candidate {
                    mode: m,
                    partition: 0,
                    rotation: 0,
                    selection: 0,
                });
                continue;
            }
            (2, false) => (0, effort.partitions2),
            (2, true) => (1, effort.partitions2),
            _ => (2, effort.partitions3),
        };
        let hi = if mode.alpha_bits > 0 { 4 } else { 3 };
        let ranking = rankings[slot].get_or_insert_with(|| ctx.rank_partitions(mode.subsets, hi));
        let count = 1 << mode.partition_bits;
        for &partition in ranking.iter().filter(|&&p| p < count).take(keep) {
            candidates.push(Candidate {
                mode: m,
                partition,
                rotation: 0,
                selection: 0,
            });
        }
    }

    let mut quick: Vec<(f32, usize, Encoding)> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let e = ctx.fit(c, QUICK);
            (e.error, i, e)
        })
        .collect();
    quick.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut best = quick[0].2;
    for &(_, i, _) in quick.iter().take(effort.finalists) {
        let e = ctx.fit(&candidates[i], effort.final_pass);
        if e.error < best.error {
            best = e;
        }
    }
    write(&best)
}

struct Ctx<'a> {
    texels: &'a [Vec4; 16],
    weights: [f32; 4],
}

/// The texels of `mask`, packed, with their block positions.
fn gather(texels: &[Vec4; 16], mask: u16) -> ([Vec4; 16], [usize; 16], usize) {
    let mut points = [[0.0; 4]; 16];
    let mut members = [0; 16];
    let mut n = 0;
    let mut bits = mask;
    while bits != 0 {
        let i = bits.trailing_zeros() as usize;
        bits &= bits - 1;
        points[n] = texels[i];
        members[n] = i;
        n += 1;
    }
    (points, members, n)
}

impl Ctx<'_> {
    /// Partitions of a `subsets`-subset mode, best line fit (over channels `0..hi`) first.
    fn rank_partitions(&self, subsets: usize, hi: usize) -> Vec<usize> {
        let moments = fit::Moments::new(self.texels, hi);
        let mut scored: Vec<(f32, usize)> = (0..64)
            .map(|p| {
                let residual = moments.partition_residual(&tables::masks(subsets, p), subsets);
                (residual, p)
            })
            .collect();
        scored.sort_by(|a, b| a.0.total_cmp(&b.0));
        scored.into_iter().map(|(_, p)| p).collect()
    }

    fn fit(&self, c: &Candidate, pass: Pass) -> Encoding {
        if MODES[c.mode].index2_bits > 0 {
            self.fit_rotated(c.mode, c.rotation, c.selection, pass)
        } else {
            self.fit_partition(c.mode, c.partition, pass)
        }
    }

    /// Modes 0–3, 6 and 7: color (and alpha, in 6 and 7) share indices.
    fn fit_partition(&self, m: usize, partition: usize, pass: Pass) -> Encoding {
        let mode = MODES[m];
        let with_alpha = mode.alpha_bits > 0;
        // Modes without alpha decode it as 255.
        let alpha_error: f32 = if with_alpha {
            0.0
        } else {
            self.texels
                .iter()
                .map(|t| self.weights[3] * (255.0 - t[3]) * (255.0 - t[3]))
                .sum()
        };
        let spec = Spec {
            lo: 0,
            hi: if with_alpha { 4 } else { 3 },
            bits: [
                mode.color_bits,
                mode.color_bits,
                mode.color_bits,
                mode.alpha_bits,
            ],
            pbits: mode.pbits,
            index_bits: mode.index_bits,
            weights: self.weights,
        };
        let mut e = Encoding {
            mode: m,
            partition,
            rotation: 0,
            index_selection: 0,
            ends: [Ends::default(); 3],
            alpha: Ends::default(),
            indices: [0; 16],
            alpha_indices: [0; 16],
            error: alpha_error,
        };
        let masks = tables::masks(mode.subsets, partition);
        let anchors = tables::anchors(mode.subsets, partition);
        for s in 0..mode.subsets {
            let (points, members, n) = gather(self.texels, masks[s]);
            let mut fitted = fit_subset(&points[..n], &spec, pass);
            // The anchor's index must have its top bit clear: mirror the subset if not.
            let anchor = members[..n].iter().position(|&i| i == anchors[s]).unwrap();
            fitted.anchor_fix(anchor, mode.index_bits);
            for (k, &i) in members[..n].iter().enumerate() {
                e.indices[i] = fitted.indices[k];
            }
            e.ends[s] = fitted.ends;
            e.error += fitted.error;
        }
        e
    }

    /// Modes 4 and 5: color and alpha fitted separately, after a channel rotation.
    fn fit_rotated(&self, m: usize, rotation: usize, selection: u32, pass: Pass) -> Encoding {
        let mode = MODES[m];
        let mut points = *self.texels;
        let mut weights = self.weights;
        if rotation > 0 {
            for p in &mut points {
                p.swap(rotation - 1, 3);
            }
            weights.swap(rotation - 1, 3);
        }
        let (color_index_bits, alpha_index_bits) = if selection == 0 {
            (mode.index_bits, mode.index2_bits)
        } else {
            (mode.index2_bits, mode.index_bits)
        };
        let color = Spec {
            lo: 0,
            hi: 3,
            bits: [mode.color_bits; 4],
            pbits: PBits::None,
            index_bits: color_index_bits,
            weights,
        };
        let alpha = Spec {
            lo: 3,
            hi: 4,
            bits: [mode.alpha_bits; 4],
            pbits: PBits::None,
            index_bits: alpha_index_bits,
            weights,
        };
        let mut c = fit_subset(&points, &color, pass);
        let mut a = fit_subset(&points, &alpha, pass);
        c.anchor_fix(0, color_index_bits);
        a.anchor_fix(0, alpha_index_bits);
        Encoding {
            mode: m,
            partition: 0,
            rotation: rotation as u32,
            index_selection: selection,
            ends: [c.ends, Ends::default(), Ends::default()],
            alpha: a.ends,
            indices: c.indices,
            alpha_indices: a.indices,
            error: c.error + a.error,
        }
    }
}

/// What one fit quantizes to: channels `lo..hi`, their bit counts (before p-bits), the p-bit
/// scheme and the index precision.
struct Spec {
    lo: usize,
    hi: usize,
    bits: [u32; 4],
    pbits: PBits,
    index_bits: u32,
    weights: [f32; 4],
}

impl Spec {
    /// The 8-bit endpoint values `ends` decodes to.
    fn unquantize(&self, ends: &Ends) -> [[u32; 4]; 2] {
        let pbit = self.pbits != PBits::None;
        core::array::from_fn(|e| {
            core::array::from_fn(|c| {
                if c < self.lo || c >= self.hi {
                    return 0;
                }
                let bits = self.bits[c];
                if pbit {
                    expand((ends.q[e][c] << 1) | ends.p[e], bits + 1)
                } else {
                    expand(ends.q[e][c], bits)
                }
            })
        })
    }

    /// The quantized value (for p-bit `p`, if any) closest to `v`.
    fn quantize(&self, v: f32, c: usize, p: Option<u32>) -> u32 {
        let bits = self.bits[c];
        let max = (1i32 << bits) - 1;
        let v = v.clamp(0.0, 255.0);
        let decoded = |q: i32| match p {
            Some(p) => expand(((q as u32) << 1) | p, bits + 1),
            None => expand(q as u32, bits),
        } as f32;
        let guess = match p {
            Some(p) => {
                let full = (1 << (bits + 1)) - 1;
                ((v * full as f32 / 255.0 - p as f32) * 0.5).round() as i32
            }
            None => (v * max as f32 / 255.0).round() as i32,
        };
        (guess - 1..=guess + 1)
            .map(|q| q.clamp(0, max))
            .min_by(|&a, &b| (decoded(a) - v).abs().total_cmp(&(decoded(b) - v).abs()))
            .unwrap() as u32
    }

    /// The p-bit combinations to try.
    fn pbit_pairs(&self) -> &'static [[u32; 2]] {
        match self.pbits {
            PBits::None => &[[0, 0]],
            PBits::Unique => &[[0, 0], [0, 1], [1, 0], [1, 1]],
            PBits::Shared => &[[0, 0], [1, 1]],
        }
    }

    /// The p-bits under which the endpoints themselves quantize with the least error.
    fn nearest_pbits(&self, e0: Vec4, e1: Vec4) -> [u32; 2] {
        let error = |e: Vec4, p: u32| -> f32 {
            (self.lo..self.hi)
                .map(|c| {
                    let q = self.quantize(e[c], c, Some(p));
                    let d = expand((q << 1) | p, self.bits[c] + 1) as f32 - e[c];
                    self.weights[c] * d * d
                })
                .sum()
        };
        match self.pbits {
            PBits::None => [0, 0],
            PBits::Unique => [e0, e1].map(|e| (error(e, 1) < error(e, 0)) as u32),
            PBits::Shared => {
                let p = (error(e0, 1) + error(e1, 1) < error(e0, 0) + error(e1, 0)) as u32;
                [p, p]
            }
        }
    }

    /// Float endpoints quantized for p-bits `p`.
    fn quantize_ends(&self, e0: Vec4, e1: Vec4, p: [u32; 2]) -> Ends {
        let pbit = |e: usize| (self.pbits != PBits::None).then_some(p[e]);
        let mut ends = Ends { q: [[0; 4]; 2], p };
        for c in self.lo..self.hi {
            ends.q[0][c] = self.quantize(e0[c], c, pbit(0));
            ends.q[1][c] = self.quantize(e1[c], c, pbit(1));
        }
        ends
    }

    /// Best indices for `points` against `ends`, and the error.
    fn assign(&self, points: &[Vec4], ends: &Ends) -> ([u32; 16], f32) {
        let [e0, e1] = self.unquantize(ends);
        let weights = weights(self.index_bits);
        let mut palette = [[0.0f32; 4]; 16];
        for (k, &w) in weights.iter().enumerate() {
            for c in self.lo..self.hi {
                palette[k][c] = interpolate(e0[c], e1[c], w) as f32;
            }
        }
        let palette = &palette[..weights.len()];
        let distance = |k: usize, p: &Vec4| -> f32 {
            let mut d = 0.0;
            for c in self.lo..self.hi {
                d += self.weights[c] * (palette[k][c] - p[c]) * (palette[k][c] - p[c]);
            }
            d
        };
        // The palette lies on the endpoint line (up to rounding), so the projection on it
        // finds the nearest entry; its neighbors cover the rounding.
        let mut axis = [0.0f32; 4];
        for c in self.lo..self.hi {
            axis[c] = e1[c] as f32 - e0[c] as f32;
        }
        let length = fit::dot(axis, axis, self.lo, self.hi);
        let last = palette.len() - 1;
        let mut indices = [0; 16];
        let mut error = 0.0;
        for (i, p) in points.iter().enumerate() {
            let (lo, hi) = if palette.len() <= 4 || length == 0.0 {
                (0, last)
            } else {
                let mut along = 0.0;
                for c in self.lo..self.hi {
                    along += (p[c] - e0[c] as f32) * axis[c];
                }
                let k = (along / length * last as f32)
                    .round()
                    .clamp(0.0, last as f32) as usize;
                (k.saturating_sub(1), (k + 1).min(last))
            };
            let mut best = (lo, distance(lo, p));
            for k in lo + 1..=hi {
                let d = distance(k, p);
                if d < best.1 {
                    best = (k, d);
                }
            }
            indices[i] = best.0 as u32;
            error += best.1;
        }
        (indices, error)
    }
}

struct Fitted {
    ends: Ends,
    /// In `points` order.
    indices: [u32; 16],
    count: usize,
    error: f32,
}

impl Fitted {
    /// Swaps the endpoints and mirrors the indices if the anchor's index has its top bit set.
    /// Interpolation weights are symmetric (`w[n - 1 - i] = 64 - w[i]`), so the decoded texels do
    /// not change.
    fn anchor_fix(&mut self, anchor: usize, index_bits: u32) {
        let top = 1 << (index_bits - 1);
        if self.indices[anchor] & top != 0 {
            self.ends.q.swap(0, 1);
            self.ends.p.swap(0, 1);
            let max = (1 << index_bits) - 1;
            for index in &mut self.indices[..self.count] {
                *index = max - *index;
            }
        }
    }
}

fn fit_subset(points: &[Vec4], spec: &Spec, pass: Pass) -> Fitted {
    let (lo, hi) = (spec.lo, spec.hi);
    let (mut e0, mut e1) = fit::axis_endpoints(points, lo, hi);
    let weights = weights(spec.index_bits);
    let mut best = Fitted {
        ends: Ends::default(),
        indices: [0; 16],
        count: points.len(),
        error: f32::MAX,
    };
    for round in 0..pass.iterations {
        let mut round_best: Option<([u32; 16], f32)> = None;
        let nearest = [spec.nearest_pbits(e0, e1)];
        let pairs = if pass.all_pbits {
            spec.pbit_pairs()
        } else {
            &nearest[..]
        };
        for &p in pairs {
            let ends = spec.quantize_ends(e0, e1, p);
            let (indices, error) = spec.assign(points, &ends);
            if error < best.error {
                best.ends = ends;
                best.indices = indices;
                best.error = error;
            }
            if round_best.is_none_or(|(_, e)| error < e) {
                round_best = Some((indices, error));
            }
        }
        if round + 1 == pass.iterations {
            break;
        }
        // Refit the float endpoints to this round's indices.
        let (indices, _) = round_best.unwrap();
        let alphas = indices.map(|i| 1.0 - weights[i as usize] as f32 / 64.0);
        match fit::least_squares(points, &alphas[..points.len()], lo, hi) {
            Some((a, b)) => (e0, e1) = (a, b),
            None => break,
        }
    }
    if pass.refine {
        refine(points, spec, &mut best);
    }
    best
}

/// Coordinate descent: steps each quantized endpoint channel by ±1 and flips p-bits while the
/// error drops.
fn refine(points: &[Vec4], spec: &Spec, best: &mut Fitted) {
    let consider = |ends: Ends, best: &mut Fitted| {
        let (indices, error) = spec.assign(points, &ends);
        if error < best.error {
            best.ends = ends;
            best.indices = indices;
            best.error = error;
            true
        } else {
            false
        }
    };
    for _ in 0..4 {
        let mut improved = false;
        for e in 0..2 {
            for c in spec.lo..spec.hi {
                let max = (1 << spec.bits[c]) - 1;
                for step in [-1i32, 1] {
                    let Some(v) = best.ends.q[e][c]
                        .checked_add_signed(step)
                        .filter(|&v| v <= max)
                    else {
                        continue;
                    };
                    let mut ends = best.ends;
                    ends.q[e][c] = v;
                    improved |= consider(ends, best);
                }
            }
            match spec.pbits {
                PBits::None => {}
                PBits::Unique => {
                    let mut ends = best.ends;
                    ends.p[e] ^= 1;
                    improved |= consider(ends, best);
                }
                PBits::Shared => {
                    if e == 0 {
                        let mut ends = best.ends;
                        ends.p = [ends.p[0] ^ 1; 2];
                        improved |= consider(ends, best);
                    }
                }
            }
        }
        if !improved {
            break;
        }
    }
}

fn write(e: &Encoding) -> [u8; 16] {
    let mode = MODES[e.mode];
    let mut w = BitWriter::default();
    w.write(1 << e.mode, e.mode as u32 + 1);
    w.write(e.partition as u32, mode.partition_bits);
    w.write(e.rotation, mode.rotation_bits);
    w.write(e.index_selection, mode.index_selection_bits);
    let subsets = &e.ends[..mode.subsets];
    for c in 0..3 {
        for q in subsets.iter().flat_map(|ends| &ends.q) {
            w.write(q[c], mode.color_bits);
        }
    }
    if mode.alpha_bits > 0 {
        // Modes 4 and 5 fit alpha on its own.
        let alpha = if mode.index2_bits > 0 {
            core::slice::from_ref(&e.alpha)
        } else {
            subsets
        };
        for q in alpha.iter().flat_map(|ends| &ends.q) {
            w.write(q[3], mode.alpha_bits);
        }
    }
    for ends in subsets {
        match mode.pbits {
            PBits::None => {}
            PBits::Unique => {
                w.write(ends.p[0], 1);
                w.write(ends.p[1], 1);
            }
            PBits::Shared => w.write(ends.p[0], 1),
        }
    }
    // Mode 4's index selection 1 stores alpha in the 2-bit (primary) set.
    let (primary, secondary) = if e.index_selection == 0 {
        (&e.indices, &e.alpha_indices)
    } else {
        (&e.alpha_indices, &e.indices)
    };
    for (i, &index) in primary.iter().enumerate() {
        let anchor = tables::is_anchor(mode.subsets, e.partition, i);
        w.write(index, mode.index_bits - anchor as u32);
    }
    if mode.index2_bits > 0 {
        for (i, &index) in secondary.iter().enumerate() {
            w.write(index, mode.index2_bits - (i == 0) as u32);
        }
    }
    w.finish()
}
