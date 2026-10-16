//! Mip chain generation.
//!
//! Each level is resampled from the one above with a separable filter, in linear light (sRGB
//! color is decoded first and re-encoded after). Sizes halve and round down, never below 1, like
//! the RHI's `TextureDesc::mip_size`, so odd and non-power-of-two sizes resample by non-integer
//! ratios: every filter is integrated over each source texel rather than point-sampled.

use crate::image::{ColorSpace, Image, Usage};
use crate::par;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum MipFilter {
    /// Area average: soft, no ringing.
    Box,
    /// Kaiser-windowed sinc (width 3, alpha 4): sharp with little ringing.
    #[default]
    Kaiser,
    /// Lanczos-windowed sinc (3 lobes): sharpest, rings the most.
    Lanczos3,
}

/// How filters read past the image edge.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum EdgeMode {
    /// Tiling textures.
    #[default]
    Wrap,
    Clamp,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct MipOptions {
    pub filter: MipFilter,
    pub edge: EdgeMode,
    /// Alpha-tested textures: scale each mip's alpha so the fraction of texels above this cutoff
    /// matches level 0. Without it, alpha-tested foliage thins out with distance.
    pub alpha_coverage: Option<f32>,
    /// At most this many levels, including level 0. `None`: down to 1×1.
    pub max_levels: Option<u32>,
}

/// Levels in a full chain down to 1×1.
pub const fn mip_level_count(width: u32, height: u32) -> u32 {
    let largest = if width > height { width } else { height };
    32 - largest.leading_zeros()
}

/// Size of mip `level`: halved per level and rounded down, at least 1.
pub const fn mip_size(width: u32, height: u32, level: u32) -> (u32, u32) {
    let w = width >> level;
    let h = height >> level;
    (if w == 0 { 1 } else { w }, if h == 0 { 1 } else { h })
}

/// The mip chain of `image`, level 0 (a copy of `image`) first. Every level keeps `image`'s
/// color space, usage and storage.
pub fn generate_mips(image: &Image, options: &MipOptions) -> Vec<Image> {
    let levels = mip_level_count(image.width, image.height)
        .min(options.max_levels.unwrap_or(u32::MAX))
        .max(1);
    let coverage = options.alpha_coverage.and_then(|cutoff| {
        let target = alpha_coverage(&image.to_f32(), cutoff);
        // Nothing to preserve when everything (or nothing) passes.
        (target > 0.0 && target < 1.0).then_some((cutoff, target))
    });

    let mut chain = Vec::with_capacity(levels as usize);
    chain.push(image.clone());
    let mut current = to_working(image);
    let (mut width, mut height) = (image.width, image.height);
    for level in 1..levels {
        let (w, h) = mip_size(image.width, image.height, level);
        let mut next = resample(&current, width, height, w, h, options);
        finish_level(&mut next, image.usage);
        if let Some((cutoff, target)) = coverage {
            scale_to_coverage(&mut next, cutoff, target);
        }
        chain.push(from_working(image, w, h, &next));
        (current, width, height) = (next, w, h);
    }
    chain
}

/// Fraction of texels whose alpha is above `cutoff`.
pub fn alpha_coverage(texels: &[[f32; 4]], cutoff: f32) -> f32 {
    let above = texels.iter().filter(|t| t[3] > cutoff).count();
    above as f32 / texels.len() as f32
}

/// Texels in the space filtering happens in: linear light, normals as signed vectors.
fn to_working(image: &Image) -> Vec<[f32; 4]> {
    let mut texels = image.texels_in(ColorSpace::Linear);
    if image.usage == Usage::Normal {
        for t in &mut texels {
            for c in &mut t[..3] {
                *c = *c * 2.0 - 1.0;
            }
        }
    }
    texels
}

fn from_working(like: &Image, width: u32, height: u32, texels: &[[f32; 4]]) -> Image {
    let mut texels = texels.to_vec();
    if like.usage == Usage::Normal {
        for t in &mut texels {
            for c in &mut t[..3] {
                *c = *c * 0.5 + 0.5;
            }
        }
    }
    like.with_texels(width, height, ColorSpace::Linear, texels)
}

/// Clamps the filters' overshoot back into range and renormalizes normals.
fn finish_level(texels: &mut [[f32; 4]], usage: Usage) {
    for t in texels {
        match usage {
            Usage::Normal => {
                let len = (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt();
                if len > 1e-6 {
                    for c in &mut t[..3] {
                        *c /= len;
                    }
                } else {
                    // Opposing normals averaged away: point straight out.
                    t[..3].copy_from_slice(&[0.0, 0.0, 1.0]);
                }
            }
            Usage::Hdr => {
                for c in &mut t[..3] {
                    *c = c.max(0.0);
                }
            }
            Usage::Color | Usage::SingleChannel => {
                for c in &mut t[..3] {
                    *c = c.clamp(0.0, 1.0);
                }
            }
        }
        t[3] = t[3].clamp(0.0, 1.0);
    }
}

/// Scales alpha so [`alpha_coverage`] at `cutoff` comes as close to `target` as it can.
fn scale_to_coverage(texels: &mut [[f32; 4]], cutoff: f32, target: f32) {
    let coverage = |scale: f32| {
        let above = texels
            .iter()
            .filter(|t| (t[3] * scale).min(1.0) > cutoff)
            .count();
        above as f32 / texels.len() as f32
    };
    // Coverage only grows with the scale: bisect for the smallest scale that reaches the target.
    let (mut lo, mut hi) = (0.0f32, 16.0f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if coverage(mid) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    // `hi` reaches the target; `lo` falls short. Keep whichever lands closer.
    let scale = if (coverage(lo) - target).abs() < (coverage(hi) - target).abs() {
        lo
    } else {
        hi
    };
    for t in texels {
        t[3] = (t[3] * scale).min(1.0);
    }
}

fn resample(
    src: &[[f32; 4]],
    width: u32,
    height: u32,
    out_width: u32,
    out_height: u32,
    options: &MipOptions,
) -> Vec<[f32; 4]> {
    let (w, h, ow, oh) = (
        width as usize,
        height as usize,
        out_width as usize,
        out_height as usize,
    );
    let horizontal = if ow == w {
        src.to_vec()
    } else {
        let taps = taps(width, out_width, options);
        let mut out = vec![[0.0; 4]; ow * h];
        par::for_each_row(&mut out, ow, |y, row| {
            let line = &src[y * w..][..w];
            for (texel, taps) in row.iter_mut().zip(&taps) {
                *texel = sum(taps.iter().map(|&(i, weight)| (line[i], weight)));
            }
        });
        out
    };
    if oh == h {
        return horizontal;
    }
    let taps = taps(height, out_height, options);
    let mut out = vec![[0.0; 4]; ow * oh];
    par::for_each_row(&mut out, ow, |y, row| {
        for (x, texel) in row.iter_mut().enumerate() {
            *texel = sum(taps[y]
                .iter()
                .map(|&(i, weight)| (horizontal[i * ow + x], weight)));
        }
    });
    out
}

fn sum(terms: impl Iterator<Item = ([f32; 4], f32)>) -> [f32; 4] {
    let mut total = [0.0; 4];
    for (texel, weight) in terms {
        for c in 0..4 {
            total[c] += texel[c] * weight;
        }
    }
    total
}

/// For each output texel along an axis, the source texels and normalized weights it sums.
fn taps(size: u32, out_size: u32, options: &MipOptions) -> Vec<Vec<(usize, f32)>> {
    // Midpoint samples per source texel when integrating the filter over it.
    const SAMPLES: usize = 8;
    let scale = size as f32 / out_size as f32;
    let radius = support(options.filter) * scale;
    (0..out_size)
        .map(|i| {
            let center = (i as f32 + 0.5) * scale;
            let first = (center - radius).floor() as i64;
            let last = (center + radius).ceil() as i64;
            let mut taps = Vec::with_capacity((last - first) as usize);
            for j in first..last {
                let mut weight = 0.0;
                for s in 0..SAMPLES {
                    let x = j as f32 + (s as f32 + 0.5) / SAMPLES as f32;
                    weight += kernel(options.filter, (x - center) / scale);
                }
                if weight != 0.0 {
                    let index = match options.edge {
                        EdgeMode::Wrap => j.rem_euclid(size as i64),
                        EdgeMode::Clamp => j.clamp(0, size as i64 - 1),
                    };
                    taps.push((index as usize, weight));
                }
            }
            let total: f32 = taps.iter().map(|t| t.1).sum();
            for t in &mut taps {
                t.1 /= total;
            }
            taps
        })
        .collect()
}

/// Half-width of the filter, in destination texels.
fn support(filter: MipFilter) -> f32 {
    match filter {
        MipFilter::Box => 0.5,
        MipFilter::Kaiser | MipFilter::Lanczos3 => 3.0,
    }
}

/// The filter at `x` destination texels from the center.
fn kernel(filter: MipFilter, x: f32) -> f32 {
    let x = x.abs();
    match filter {
        MipFilter::Box => {
            if x < 0.5 {
                1.0
            } else {
                0.0
            }
        }
        MipFilter::Kaiser => {
            const WIDTH: f32 = 3.0;
            const ALPHA: f32 = 4.0;
            if x >= WIDTH {
                return 0.0;
            }
            let t = x / WIDTH;
            sinc(x) * bessel_i0(ALPHA * (1.0 - t * t).sqrt()) / bessel_i0(ALPHA)
        }
        MipFilter::Lanczos3 => {
            if x >= 3.0 {
                0.0
            } else {
                sinc(x) * sinc(x / 3.0)
            }
        }
    }
}

fn sinc(x: f32) -> f32 {
    if x < 1e-5 {
        1.0
    } else {
        let px = core::f32::consts::PI * x;
        px.sin() / px
    }
}

/// Modified Bessel function of the first kind, order 0 (power series).
fn bessel_i0(x: f32) -> f32 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x * 0.5;
    for k in 1..32 {
        term *= half / k as f32;
        let t = term * term;
        sum += t;
        if t < sum * 1e-8 {
            break;
        }
    }
    sum
}
