//! BC4 channel blocks: BC4, both halves of BC5, and BC3's alpha.
//!
//! Two 8-bit endpoints and 3-bit indices. `r0 > r1` selects eight values (the endpoints and six
//! interpolated); otherwise six (four interpolated) plus exact 0 and 255.

use super::fit;
use crate::format::{Interpolation, Quality};

pub(crate) struct Params {
    pub quality: Quality,
    pub mode_mask: u32,
}

/// The palette, 0–255.
fn palette(r0: u8, r1: u8) -> [f32; 8] {
    let (a, b) = (r0 as f32, r1 as f32);
    let mut p = [a, b, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    if r0 > r1 {
        for (i, v) in p.iter_mut().enumerate().skip(2) {
            *v = ((8 - i) as f32 * a + (i - 1) as f32 * b) / 7.0;
        }
    } else {
        for (i, v) in p.iter_mut().enumerate().take(6).skip(2) {
            *v = ((6 - i) as f32 * a + (i - 1) as f32 * b) / 5.0;
        }
        p[7] = 255.0;
    }
    p
}

/// NVIDIA's palette in 16-bit unorm units: `r0 * w0 + r1 * w1` with weights summing to 257 (so
/// endpoints land exactly on `r * 257`), not quite the spec's sevenths and fifths.
fn palette_nvidia(r0: u8, r1: u8) -> [f32; 8] {
    const EIGHT: [[u32; 2]; 8] = [
        [257, 0],
        [0, 257],
        [221, 36],
        [185, 72],
        [144, 113],
        [113, 144],
        [72, 185],
        [36, 221],
    ];
    const SIX: [[u32; 2]; 6] = [
        [257, 0],
        [0, 257],
        [209, 48],
        [161, 96],
        [96, 161],
        [48, 209],
    ];
    let (a, b) = (r0 as u32, r1 as u32);
    let mut p = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 65535.0];
    if r0 > r1 {
        for (v, [wa, wb]) in p.iter_mut().zip(EIGHT) {
            *v = (a * wa + b * wb) as f32;
        }
    } else {
        for (v, [wa, wb]) in p.iter_mut().zip(SIX) {
            *v = (a * wa + b * wb) as f32;
        }
    }
    p
}

/// NVIDIA's palette for BC3 alpha, which it decodes to 8 bits: `(r0 * w0 + r1 * w1 + 1024) >>
/// 11`, again not quite the spec's fractions.
fn palette_nvidia_alpha(r0: u8, r1: u8) -> [f32; 8] {
    const EIGHT: [[u32; 2]; 8] = [
        [2048, 0],
        [0, 2048],
        [1759, 289],
        [1470, 578],
        [1156, 892],
        [892, 1156],
        [578, 1470],
        [289, 1759],
    ];
    const SIX: [[u32; 2]; 6] = [
        [2048, 0],
        [0, 2048],
        [1663, 385],
        [1278, 770],
        [770, 1278],
        [385, 1663],
    ];
    let (a, b) = (r0 as u32, r1 as u32);
    let mut p = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 255.0];
    let weights: &[[u32; 2]] = if r0 > r1 { &EIGHT } else { &SIX };
    for (v, [wa, wb]) in p.iter_mut().zip(weights) {
        *v = ((a * wa + b * wb + 1024) >> 11) as f32;
    }
    p
}

/// Values in [0, 1]. `bc3_alpha`: the block is BC3's alpha, which NVIDIA decodes differently.
pub(crate) fn decode(block: &[u8], interpolation: Interpolation, bc3_alpha: bool) -> [f32; 16] {
    let (palette, scale) = match (interpolation, bc3_alpha) {
        (Interpolation::Exact, _) => (palette(block[0], block[1]), 255.0),
        (Interpolation::Nvidia, false) => (palette_nvidia(block[0], block[1]), 65535.0),
        (Interpolation::Nvidia, true) => (palette_nvidia_alpha(block[0], block[1]), 255.0),
    };
    let mut bits = [0u8; 8];
    bits[..6].copy_from_slice(&block[2..8]);
    let bits = u64::from_le_bytes(bits);
    core::array::from_fn(|i| palette[(bits >> (3 * i)) as usize & 7] / scale)
}

#[derive(Clone, Copy)]
struct Candidate {
    r0: u8,
    r1: u8,
    indices: u64,
    error: f32,
}

fn evaluate(values: &[f32; 16], r0: u8, r1: u8) -> Candidate {
    let palette = palette(r0, r1);
    let mut indices = 0;
    let mut error = 0.0;
    for (i, &v) in values.iter().enumerate() {
        let mut best = (0, f32::MAX);
        for (k, &p) in palette.iter().enumerate() {
            let d = (p - v) * (p - v);
            if d < best.1 {
                best = (k, d);
            }
        }
        indices |= (best.0 as u64) << (3 * i);
        error += best.1;
    }
    Candidate {
        r0,
        r1,
        indices,
        error,
    }
}

/// Values 0–255.
pub(crate) fn encode(values: &[f32; 16], params: &Params) -> [u8; 8] {
    let mut modes = Vec::with_capacity(2);
    if params.mode_mask & 1 != 0 {
        modes.push(true);
    }
    if params.mode_mask & 2 != 0 {
        modes.push(false);
    }
    if modes.is_empty() {
        modes.push(true);
    }
    let (iterations, radius) = match params.quality {
        Quality::Fast => (1, 0),
        Quality::Normal => (2, 1),
        Quality::High => (4, 4),
    };

    let mut best = Candidate {
        r0: 0,
        r1: 0,
        indices: 0,
        error: f32::MAX,
    };
    for &eight in &modes {
        let round = |v: f32| v.round().clamp(0.0, 255.0) as u8;
        // Endpoint order that selects the mode (equal endpoints decode as six values).
        let order = |a: u8, b: u8| {
            if eight {
                (a.max(b), a.min(b))
            } else {
                (a.min(b), a.max(b))
            }
        };
        let mut consider = |a: u8, b: u8| {
            let (r0, r1) = order(a, b);
            let candidate = evaluate(values, r0, r1);
            if candidate.error < best.error {
                best = candidate;
            }
            candidate
        };

        // Six-value mode spans only the values that 0 and 255 do not already cover.
        let span: Vec<f32> = values
            .iter()
            .copied()
            .filter(|&v| eight || (0.5..254.5).contains(&v))
            .collect();
        let (lo, hi) = span
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        let (lo, hi) = if span.is_empty() {
            (0.0, 0.0)
        } else {
            (lo, hi)
        };
        let mut current = consider(round(hi), round(lo));

        let alpha = |index: usize| -> Option<f32> {
            let a = match (eight, index) {
                (_, 0) => 1.0,
                (_, 1) => 0.0,
                (true, i) => (8 - i) as f32 / 7.0,
                (false, i @ 2..=5) => (6 - i) as f32 / 5.0,
                // The constant 0 and 255 do not depend on the endpoints.
                (false, _) => return None,
            };
            Some(a)
        };
        for _ in 0..iterations {
            let mut points = [[0.0; 4]; 16];
            let mut alphas = [0.0; 16];
            let mut n = 0;
            for (i, &v) in values.iter().enumerate() {
                if let Some(a) = alpha((current.indices >> (3 * i)) as usize & 7) {
                    points[n][0] = v;
                    alphas[n] = a;
                    n += 1;
                }
            }
            let Some((a, b)) = fit::least_squares(&points[..n], &alphas[..n], 0, 1) else {
                break;
            };
            let next = consider(round(a[0]), round(b[0]));
            if next.error >= current.error {
                break;
            }
            current = next;
        }

        // Exhaustive search of a window around the best endpoints of this mode.
        if radius > 0 {
            let center = current;
            for d0 in -radius..=radius {
                for d1 in -radius..=radius {
                    let a = (center.r0 as i32 + d0).clamp(0, 255) as u8;
                    let b = (center.r1 as i32 + d1).clamp(0, 255) as u8;
                    consider(a, b);
                }
            }
        }
    }

    let mut out = [0; 8];
    out[0] = best.r0;
    out[1] = best.r1;
    out[2..].copy_from_slice(&best.indices.to_le_bytes()[..6]);
    out
}
