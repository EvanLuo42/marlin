//! BC6H: HDR RGB half floats, unsigned or signed. Fourteen modes trade one or two subsets,
//! endpoint precision and delta-coded ("transformed") endpoints.
//!
//! The encoder works on half-float bit patterns as integers, which grow roughly with the
//! logarithm of the value, so its squared error weighs relative rather than absolute error.

use super::bc7::{interpolate_signed, weights};
use super::bits::{BitReader, BitWriter};
use super::fit::{self, Vec4};
use super::tables;
use crate::color::f32_to_f16;
use crate::format::Quality;

// Header fields: endpoint (w, x, y, z) * 3 + channel (r, g, b), then the partition.
const RW: u8 = 0;
const GW: u8 = 1;
const BW: u8 = 2;
const RX: u8 = 3;
const GX: u8 = 4;
const BX: u8 = 5;
const RY: u8 = 6;
const GY: u8 = 7;
const BY: u8 = 8;
const RZ: u8 = 9;
const GZ: u8 = 10;
const BZ: u8 = 11;
const D: u8 = 12;

/// A run of header bits: field, then the spec's `[a:b]`. The stream holds bit `b` first and
/// steps toward bit `a`, so `[9:0]` ascends and `[10:15]` descends.
type Run = (u8, u8, u8);

struct Mode {
    /// Mode bits, read LSB first: 2 bits for modes 1 and 2, else 5.
    code: u32,
    code_bits: u32,
    subsets: usize,
    transformed: bool,
    /// Endpoint precision.
    prec: u32,
    /// Bits of the other endpoints per channel: deltas when transformed.
    delta: [u32; 3],
    layout: &'static [Run],
}

#[rustfmt::skip]
const MODES: [Mode; 14] = [
    Mode { code: 0b00, code_bits: 2, subsets: 2, transformed: true, prec: 10, delta: [5, 5, 5], layout: &[
        (GY, 4, 4), (BY, 4, 4), (BZ, 4, 4), (RW, 9, 0), (GW, 9, 0), (BW, 9, 0), (RX, 4, 0),
        (GZ, 4, 4), (GY, 3, 0), (GX, 4, 0), (BZ, 0, 0), (GZ, 3, 0), (BX, 4, 0), (BZ, 1, 1),
        (BY, 3, 0), (RY, 4, 0), (BZ, 2, 2), (RZ, 4, 0), (BZ, 3, 3), (D, 4, 0),
    ] },
    Mode { code: 0b01, code_bits: 2, subsets: 2, transformed: true, prec: 7, delta: [6, 6, 6], layout: &[
        (GY, 5, 5), (GZ, 4, 4), (GZ, 5, 5), (RW, 6, 0), (BZ, 0, 0), (BZ, 1, 1), (BY, 4, 4),
        (GW, 6, 0), (BY, 5, 5), (BZ, 2, 2), (GY, 4, 4), (BW, 6, 0), (BZ, 3, 3), (BZ, 5, 5),
        (BZ, 4, 4), (RX, 5, 0), (GY, 3, 0), (GX, 5, 0), (GZ, 3, 0), (BX, 5, 0), (BY, 3, 0),
        (RY, 5, 0), (RZ, 5, 0), (D, 4, 0),
    ] },
    Mode { code: 0b00010, code_bits: 5, subsets: 2, transformed: true, prec: 11, delta: [5, 4, 4], layout: &[
        (RW, 9, 0), (GW, 9, 0), (BW, 9, 0), (RX, 4, 0), (RW, 10, 10), (GY, 3, 0), (GX, 3, 0),
        (GW, 10, 10), (BZ, 0, 0), (GZ, 3, 0), (BX, 3, 0), (BW, 10, 10), (BZ, 1, 1), (BY, 3, 0),
        (RY, 4, 0), (BZ, 2, 2), (RZ, 4, 0), (BZ, 3, 3), (D, 4, 0),
    ] },
    Mode { code: 0b00110, code_bits: 5, subsets: 2, transformed: true, prec: 11, delta: [4, 5, 4], layout: &[
        (RW, 9, 0), (GW, 9, 0), (BW, 9, 0), (RX, 3, 0), (RW, 10, 10), (GZ, 4, 4), (GY, 3, 0),
        (GX, 4, 0), (GW, 10, 10), (GZ, 3, 0), (BX, 3, 0), (BW, 10, 10), (BZ, 1, 1), (BY, 3, 0),
        (RY, 3, 0), (BZ, 0, 0), (BZ, 2, 2), (RZ, 3, 0), (GY, 4, 4), (BZ, 3, 3), (D, 4, 0),
    ] },
    Mode { code: 0b01010, code_bits: 5, subsets: 2, transformed: true, prec: 11, delta: [4, 4, 5], layout: &[
        (RW, 9, 0), (GW, 9, 0), (BW, 9, 0), (RX, 3, 0), (RW, 10, 10), (BY, 4, 4), (GY, 3, 0),
        (GX, 3, 0), (GW, 10, 10), (BZ, 0, 0), (GZ, 3, 0), (BX, 4, 0), (BW, 10, 10), (BY, 3, 0),
        (RY, 3, 0), (BZ, 1, 1), (BZ, 2, 2), (RZ, 3, 0), (BZ, 4, 4), (BZ, 3, 3), (D, 4, 0),
    ] },
    Mode { code: 0b01110, code_bits: 5, subsets: 2, transformed: true, prec: 9, delta: [5, 5, 5], layout: &[
        (RW, 8, 0), (BY, 4, 4), (GW, 8, 0), (GY, 4, 4), (BW, 8, 0), (BZ, 4, 4), (RX, 4, 0),
        (GZ, 4, 4), (GY, 3, 0), (GX, 4, 0), (BZ, 0, 0), (GZ, 3, 0), (BX, 4, 0), (BZ, 1, 1),
        (BY, 3, 0), (RY, 4, 0), (BZ, 2, 2), (RZ, 4, 0), (BZ, 3, 3), (D, 4, 0),
    ] },
    Mode { code: 0b10010, code_bits: 5, subsets: 2, transformed: true, prec: 8, delta: [6, 5, 5], layout: &[
        (RW, 7, 0), (GZ, 4, 4), (BY, 4, 4), (GW, 7, 0), (BZ, 2, 2), (GY, 4, 4), (BW, 7, 0),
        (BZ, 3, 3), (BZ, 4, 4), (RX, 5, 0), (GY, 3, 0), (GX, 4, 0), (BZ, 0, 0), (GZ, 3, 0),
        (BX, 4, 0), (BZ, 1, 1), (BY, 3, 0), (RY, 5, 0), (RZ, 5, 0), (D, 4, 0),
    ] },
    Mode { code: 0b10110, code_bits: 5, subsets: 2, transformed: true, prec: 8, delta: [5, 6, 5], layout: &[
        (RW, 7, 0), (BZ, 0, 0), (BY, 4, 4), (GW, 7, 0), (GY, 5, 5), (GY, 4, 4), (BW, 7, 0),
        (GZ, 5, 5), (BZ, 4, 4), (RX, 4, 0), (GZ, 4, 4), (GY, 3, 0), (GX, 5, 0), (GZ, 3, 0),
        (BX, 4, 0), (BZ, 1, 1), (BY, 3, 0), (RY, 4, 0), (BZ, 2, 2), (RZ, 4, 0), (BZ, 3, 3),
        (D, 4, 0),
    ] },
    Mode { code: 0b11010, code_bits: 5, subsets: 2, transformed: true, prec: 8, delta: [5, 5, 6], layout: &[
        (RW, 7, 0), (BZ, 1, 1), (BY, 4, 4), (GW, 7, 0), (BY, 5, 5), (GY, 4, 4), (BW, 7, 0),
        (BZ, 5, 5), (BZ, 4, 4), (RX, 4, 0), (GZ, 4, 4), (GY, 3, 0), (GX, 4, 0), (BZ, 0, 0),
        (GZ, 3, 0), (BX, 5, 0), (BY, 3, 0), (RY, 4, 0), (BZ, 2, 2), (RZ, 4, 0), (BZ, 3, 3),
        (D, 4, 0),
    ] },
    Mode { code: 0b11110, code_bits: 5, subsets: 2, transformed: false, prec: 6, delta: [6, 6, 6], layout: &[
        (RW, 5, 0), (GZ, 4, 4), (BZ, 0, 0), (BZ, 1, 1), (BY, 4, 4), (GW, 5, 0), (GY, 5, 5),
        (BY, 5, 5), (BZ, 2, 2), (GY, 4, 4), (BW, 5, 0), (GZ, 5, 5), (BZ, 3, 3), (BZ, 5, 5),
        (BZ, 4, 4), (RX, 5, 0), (GY, 3, 0), (GX, 5, 0), (GZ, 3, 0), (BX, 5, 0), (BY, 3, 0),
        (RY, 5, 0), (RZ, 5, 0), (D, 4, 0),
    ] },
    Mode { code: 0b00011, code_bits: 5, subsets: 1, transformed: false, prec: 10, delta: [10, 10, 10], layout: &[
        (RW, 9, 0), (GW, 9, 0), (BW, 9, 0), (RX, 9, 0), (GX, 9, 0), (BX, 9, 0),
    ] },
    Mode { code: 0b00111, code_bits: 5, subsets: 1, transformed: true, prec: 11, delta: [9, 9, 9], layout: &[
        (RW, 9, 0), (GW, 9, 0), (BW, 9, 0), (RX, 8, 0), (RW, 10, 10), (GX, 8, 0), (GW, 10, 10),
        (BX, 8, 0), (BW, 10, 10),
    ] },
    Mode { code: 0b01011, code_bits: 5, subsets: 1, transformed: true, prec: 12, delta: [8, 8, 8], layout: &[
        (RW, 9, 0), (GW, 9, 0), (BW, 9, 0), (RX, 7, 0), (RW, 10, 11), (GX, 7, 0), (GW, 10, 11),
        (BX, 7, 0), (BW, 10, 11),
    ] },
    Mode { code: 0b01111, code_bits: 5, subsets: 1, transformed: true, prec: 16, delta: [4, 4, 4], layout: &[
        (RW, 9, 0), (GW, 9, 0), (BW, 9, 0), (RX, 3, 0), (RW, 10, 15), (GX, 3, 0), (GW, 10, 15),
        (BX, 3, 0), (BW, 10, 15),
    ] },
];

/// Bit positions of a run, in stream order.
fn run_bits(a: u8, b: u8) -> impl Iterator<Item = u8> {
    let up = (a >= b).then_some(b..=a);
    let down = (a < b).then(|| (a..=b).rev());
    up.into_iter().flatten().chain(down.into_iter().flatten())
}

/// The mode index (spec mode − 1) of an encoded block; `None` for the reserved modes.
pub(crate) fn block_mode(block: &[u8; 16]) -> Option<usize> {
    let low = block[0] & 3;
    if low < 2 {
        return Some(low as usize);
    }
    let code = (block[0] & 31) as u32;
    MODES
        .iter()
        .position(|m| m.code_bits == 5 && m.code == code)
}

fn sign_extend(v: i32, bits: u32) -> i32 {
    if bits >= 32 {
        v
    } else {
        (v << (32 - bits)) >> (32 - bits)
    }
}

/// An endpoint value of `prec` bits widened to 16 (signed: 15 plus sign).
fn unquantize(v: i32, prec: u32, signed: bool) -> i32 {
    if !signed {
        if prec >= 15 {
            v
        } else if v == 0 {
            0
        } else if v == (1 << prec) - 1 {
            0xffff
        } else {
            ((v << 16) + 0x8000) >> prec
        }
    } else if prec >= 16 {
        v
    } else {
        let m = v.abs();
        let u = if m == 0 {
            0
        } else if m >= (1 << (prec - 1)) - 1 {
            0x7fff
        } else {
            ((m << 15) + 0x4000) >> (prec - 1)
        };
        if v < 0 { -u } else { u }
    }
}

/// An interpolated 16-bit value to half-float bits.
fn finish(v: i32, signed: bool) -> u16 {
    if !signed {
        ((v * 31) >> 6) as u16
    } else if v < 0 {
        0x8000 | ((-v * 31) >> 5) as u16
    } else {
        ((v * 31) >> 5) as u16
    }
}

/// Half bits as an integer that orders like the value: the magnitude, negated for negatives.
fn half_to_int(h: u16) -> i32 {
    let m = (h & 0x7fff) as i32;
    if h & 0x8000 != 0 { -m } else { m }
}

/// Texels as half-float RGB bits.
pub(crate) fn decode(block: &[u8; 16], signed: bool) -> [[u16; 3]; 16] {
    let Some(m) = block_mode(block) else {
        return [[0; 3]; 16];
    };
    let mode = &MODES[m];
    let mut r = BitReader::new(block);
    r.read(mode.code_bits);
    let mut fields = [0u32; 13];
    for &(f, a, b) in mode.layout {
        for bit in run_bits(a, b) {
            fields[f as usize] |= r.read(1) << bit;
        }
    }
    let partition = fields[D as usize] as usize;
    let count = mode.subsets * 2;
    let mut ends: [[i32; 3]; 4] =
        core::array::from_fn(|e| core::array::from_fn(|c| fields[e * 3 + c] as i32));
    let mask = ((1u32 << mode.prec) - 1) as i32;
    for c in 0..3 {
        if signed {
            ends[0][c] = sign_extend(ends[0][c], mode.prec);
        }
        let base = ends[0][c];
        for end in &mut ends[1..count] {
            if signed || mode.transformed {
                end[c] = sign_extend(end[c], mode.delta[c]);
            }
            if mode.transformed {
                end[c] = (base + end[c]) & mask;
                if signed {
                    end[c] = sign_extend(end[c], mode.prec);
                }
            }
        }
    }
    let ends = ends.map(|e| e.map(|v| unquantize(v, mode.prec, signed)));

    let index_bits = if mode.subsets == 1 { 4 } else { 3 };
    let weights = weights(index_bits);
    core::array::from_fn(|i| {
        let anchor = tables::is_anchor(mode.subsets, partition, i);
        let index = r.read(index_bits - anchor as u32) as usize;
        let s = tables::subset(mode.subsets, partition, i);
        core::array::from_fn(|c| {
            let v = interpolate_signed(ends[2 * s][c], ends[2 * s + 1][c], weights[index]);
            finish(v, signed)
        })
    })
}

pub(crate) struct Params {
    pub quality: Quality,
    pub weights: [f32; 4],
    pub signed: bool,
    pub mode_mask: u32,
}

struct Encoding {
    mode: usize,
    partition: usize,
    /// Decoded endpoint values (`prec` bits, after the delta transform), w x y z.
    ends: [[i32; 3]; 4],
    indices: [u32; 16],
    error: f32,
}

/// Linear RGB texels.
pub(crate) fn encode(texels: &[Vec4; 16], params: &Params) -> [u8; 16] {
    let signed = params.signed;
    // Targets as ordered half integers, and the 16-bit interpolation domain they come from.
    let targets: [[i32; 3]; 16] = core::array::from_fn(|i| {
        core::array::from_fn(|c| {
            let v = texels[i][c];
            let v = if v.is_nan() { 0.0 } else { v };
            let v = if signed {
                v.clamp(-65504.0, 65504.0)
            } else {
                v.clamp(0.0, 65504.0)
            };
            half_to_int(f32_to_f16(v))
        })
    });
    let scale = if signed { 32.0 / 31.0 } else { 64.0 / 31.0 };
    let points: [Vec4; 16] = core::array::from_fn(|i| {
        let t = targets[i];
        [
            t[0] as f32 * scale,
            t[1] as f32 * scale,
            t[2] as f32 * scale,
            0.0,
        ]
    });

    // Every (mode, partition) candidate gets a quick fit (one least-squares refit); the best few
    // are refitted with more rounds (and, at High, a neighbor search).
    let (default_modes, partitions, finalists, iterations, refine) = match params.quality {
        // Mode 1 and the single-subset modes 11 to 14.
        Quality::Fast => (0b11_1100_0000_0001u32, 1, 1, 2, false),
        Quality::Normal => (0x3fff, 4, 3, 3, false),
        Quality::High => (0x3fff, 12, 4, 4, true),
    };
    let mut modes = default_modes & params.mode_mask;
    if modes == 0 {
        modes = params.mode_mask & 0x3fff;
    }
    assert!(modes != 0, "BC6H mode mask allows no mode");

    let ctx = Ctx {
        targets: &targets,
        points: &points,
        weights: params.weights,
        signed,
    };
    // Two-subset partitions ranked once for every two-subset mode.
    let moments = fit::Moments::new(&points, 3);
    let mut ranked: Vec<(f32, usize)> = (0..32)
        .map(|p| (moments.partition_residual(&tables::masks(2, p), 2), p))
        .collect();
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut candidates = Vec::new();
    for m in (0..14).filter(|m| modes & (1 << m) != 0) {
        if MODES[m].subsets == 1 {
            candidates.push((m, 0));
        } else {
            candidates.extend(ranked.iter().take(partitions).map(|r| (m, r.1)));
        }
    }
    let mut quick: Vec<Encoding> = candidates
        .iter()
        .map(|&(m, p)| ctx.fit(m, p, 2, false))
        .collect();
    quick.sort_by(|a, b| a.error.total_cmp(&b.error));
    let mut best = quick.swap_remove(0);
    let finals: Vec<(usize, usize)> = core::iter::once((best.mode, best.partition))
        .chain(quick.iter().map(|e| (e.mode, e.partition)))
        .take(finalists)
        .collect();
    for (m, p) in finals {
        let e = ctx.fit(m, p, iterations, refine);
        if e.error < best.error {
            best = e;
        }
    }
    write(&best)
}

struct Ctx<'a> {
    targets: &'a [[i32; 3]; 16],
    points: &'a [Vec4; 16],
    weights: [f32; 4],
    signed: bool,
}

impl Ctx<'_> {
    fn domain(&self) -> (f32, f32) {
        if self.signed {
            (-32767.0, 32767.0)
        } else {
            (0.0, 65535.0)
        }
    }

    /// The `prec`-bit value that unquantizes closest to `u`.
    fn quantize(&self, u: f32, prec: u32) -> i32 {
        let (lo, hi, guess) = if self.signed {
            let max = (1 << (prec - 1)) - 1;
            (-max, max, (u * (1 << (prec - 1)) as f32 / 32768.0) as i32)
        } else {
            let max = ((1u32 << prec) - 1) as i32;
            (0, max, (u * (1u32 << prec) as f32 / 65536.0) as i32)
        };
        (guess - 1..=guess + 1)
            .map(|q| q.clamp(lo, hi))
            .min_by_key(|&q| (unquantize(q, prec, self.signed) as f32 - u).abs() as i64)
            .unwrap()
    }

    fn fit(&self, m: usize, partition: usize, iterations: u32, refine: bool) -> Encoding {
        let mode = &MODES[m];
        let (lo, hi) = self.domain();
        let masks = tables::masks(mode.subsets, partition);
        let mut subsets = [([[0.0f32; 4]; 16], [0usize; 16], 0usize); 2];
        let mut floats = [([0.0f32; 4], [0.0f32; 4]); 2];
        for s in 0..mode.subsets {
            let mut bits = masks[s];
            let (points, members, n) = &mut subsets[s];
            while bits != 0 {
                let i = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                points[*n] = self.points[i];
                members[*n] = i;
                *n += 1;
            }
            floats[s] = fit::axis_endpoints(&points[..*n], 0, 3);
        }
        let index_bits = if mode.subsets == 1 { 4 } else { 3 };
        let weights = weights(index_bits);

        let mut best: Option<Encoding> = None;
        for round in 0..iterations {
            let mut q = [[0i32; 3]; 4];
            for (s, (e0, e1)) in floats.iter().enumerate().take(mode.subsets) {
                for c in 0..3 {
                    q[2 * s][c] = self.quantize(e0[c].clamp(lo, hi), mode.prec);
                    q[2 * s + 1][c] = self.quantize(e1[c].clamp(lo, hi), mode.prec);
                }
            }
            let e = self.finalize(m, partition, q);
            if round + 1 < iterations {
                // Refit the float endpoints to these indices.
                for (s, (points, members, n)) in subsets.iter().enumerate().take(mode.subsets) {
                    let alphas =
                        members.map(|i| 1.0 - weights[e.indices[i] as usize] as f32 / 64.0);
                    if let Some(ends) = fit::least_squares(&points[..*n], &alphas[..*n], 0, 3) {
                        floats[s] = ends;
                    }
                }
            }
            if best.as_ref().is_none_or(|b| e.error < b.error) {
                best = Some(e);
            }
        }
        let mut best = best.unwrap();

        if refine {
            let count = mode.subsets * 2;
            let max = if self.signed {
                (1 << (mode.prec - 1)) - 1
            } else {
                ((1u32 << mode.prec) - 1) as i32
            };
            let min = if self.signed { -max } else { 0 };
            for _ in 0..4 {
                let mut improved = false;
                for e in 0..count {
                    for c in 0..3 {
                        for step in [-1, 1] {
                            let mut q = best.ends;
                            q[e][c] = (q[e][c] + step).clamp(min, max);
                            let candidate = self.finalize(m, partition, q);
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
        }
        best
    }

    /// Orders each subset's endpoints for its anchor, applies the delta transform's range
    /// (clamping deltas that do not fit), and assigns indices against what will decode.
    fn finalize(&self, m: usize, partition: usize, mut q: [[i32; 3]; 4]) -> Encoding {
        let mode = &MODES[m];
        let anchors = tables::anchors(mode.subsets, partition);
        for s in 0..mode.subsets {
            // The anchor's index must have its top bit clear: put the endpoint it is nearer
            // first. Index assignment below enforces it where this guess is wrong.
            let (a, b) = (q[2 * s], q[2 * s + 1]);
            let t = self.points[anchors[s]];
            let (mut along, mut length) = (0.0, 0.0);
            for c in 0..3 {
                let ua = unquantize(a[c], mode.prec, self.signed) as f32;
                let d = unquantize(b[c], mode.prec, self.signed) as f32 - ua;
                along += (t[c] - ua) * d;
                length += d * d;
            }
            if along > 0.5 * length {
                q.swap(2 * s, 2 * s + 1);
            }
        }
        if mode.transformed {
            for end in 1..mode.subsets * 2 {
                for c in 0..3 {
                    let bits = mode.delta[c];
                    let (lo, hi) = (-(1 << (bits - 1)), (1 << (bits - 1)) - 1);
                    q[end][c] = q[0][c] + (q[end][c] - q[0][c]).clamp(lo, hi);
                }
            }
        }
        let mut indices = [0; 16];
        let mut error = 0.0;
        for s in 0..mode.subsets {
            let (subset_indices, subset_error) =
                self.assign(mode, partition, s, q[2 * s], q[2 * s + 1], Some(anchors[s]));
            for i in 0..16 {
                if tables::subset(mode.subsets, partition, i) == s {
                    indices[i] = subset_indices[i];
                }
            }
            error += subset_error;
        }
        Encoding {
            mode: m,
            partition,
            ends: q,
            indices,
            error,
        }
    }

    /// Best indices of subset `s`'s texels for endpoints `a`, `b` (`prec`-bit), and the error.
    /// With `anchor`, that texel only takes indices whose top bit is clear.
    fn assign(
        &self,
        mode: &Mode,
        partition: usize,
        s: usize,
        a: [i32; 3],
        b: [i32; 3],
        anchor: Option<usize>,
    ) -> ([u32; 16], f32) {
        let index_bits = if mode.subsets == 1 { 4 } else { 3 };
        let weights = weights(index_bits);
        let a = a.map(|v| unquantize(v, mode.prec, self.signed));
        let b = b.map(|v| unquantize(v, mode.prec, self.signed));
        let mut palette = [[0f32; 3]; 16];
        for (k, &w) in weights.iter().enumerate() {
            for c in 0..3 {
                let v = interpolate_signed(a[c], b[c], w);
                palette[k][c] = half_to_int(finish(v, self.signed)) as f32;
            }
        }
        let distance = |k: usize, t: [i32; 3]| -> f32 {
            let mut d = 0.0;
            for c in 0..3 {
                let e = palette[k][c] - t[c] as f32;
                d += self.weights[c] * e * e;
            }
            d
        };
        // The palette lies on a line (the unquantize, interpolate, finish chain is linear up to
        // rounding): project for the nearest entry and check its neighbors.
        let last = weights.len() - 1;
        let axis: [f32; 3] = core::array::from_fn(|c| palette[last][c] - palette[0][c]);
        let length: f32 = axis.iter().map(|d| d * d).sum();
        let mut indices = [0; 16];
        let mut error = 0.0;
        let mut bits = tables::masks(mode.subsets, partition)[s];
        while bits != 0 {
            let i = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            let usable = if anchor == Some(i) { last / 2 } else { last };
            let t = self.targets[i];
            let (lo, hi) = if length == 0.0 {
                (0, 0)
            } else {
                let along: f32 = (0..3)
                    .map(|c| (t[c] as f32 - palette[0][c]) * axis[c])
                    .sum();
                let k = (along / length * last as f32)
                    .round()
                    .clamp(0.0, usable as f32) as usize;
                (k.saturating_sub(1), (k + 1).min(usable))
            };
            let mut best = (lo, distance(lo, t));
            for k in lo + 1..=hi {
                let d = distance(k, t);
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

fn write(e: &Encoding) -> [u8; 16] {
    let mode = &MODES[e.mode];
    let mut fields = [0u32; 13];
    let mask = |bits: u32| ((1u64 << bits) - 1) as u32;
    for c in 0..3 {
        fields[c] = e.ends[0][c] as u32 & mask(mode.prec);
        for end in 1..mode.subsets * 2 {
            let v = if mode.transformed {
                e.ends[end][c] - e.ends[0][c]
            } else {
                e.ends[end][c]
            };
            fields[end * 3 + c] = v as u32 & mask(mode.delta[c]);
        }
    }
    fields[D as usize] = e.partition as u32;

    let mut w = BitWriter::default();
    w.write(mode.code, mode.code_bits);
    for &(f, a, b) in mode.layout {
        for bit in run_bits(a, b) {
            w.write((fields[f as usize] >> bit) & 1, 1);
        }
    }
    let index_bits = if mode.subsets == 1 { 4 } else { 3 };
    for (i, &index) in e.indices.iter().enumerate() {
        let anchor = tables::is_anchor(mode.subsets, e.partition, i);
        w.write(index, index_bits - anchor as u32);
    }
    w.finish()
}
