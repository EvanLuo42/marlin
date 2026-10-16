//! Error metrics between a source and its decoded encoding.

use core::ops::Range;

/// Mean squared error over `channels` of row-major texels.
pub fn mse(a: &[[f32; 4]], b: &[[f32; 4]], channels: Range<usize>) -> f64 {
    assert_eq!(a.len(), b.len(), "texel counts");
    let mut sum = 0.0f64;
    for (x, y) in a.iter().zip(b) {
        for c in channels.clone() {
            let d = (x[c] - y[c]) as f64;
            sum += d * d;
        }
    }
    sum / (a.len() * channels.len()).max(1) as f64
}

pub fn rmse(a: &[[f32; 4]], b: &[[f32; 4]], channels: Range<usize>) -> f64 {
    mse(a, b, channels).sqrt()
}

/// Peak signal-to-noise ratio in dB for values in `[0, peak]`; infinite for identical inputs.
pub fn psnr(a: &[[f32; 4]], b: &[[f32; 4]], channels: Range<usize>, peak: f64) -> f64 {
    let mse = mse(a, b, channels);
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (peak * peak / mse).log10()
    }
}

/// Multi-exposure PSNR of HDR RGB (Munkberg et al. 2006): both images exposed at −4 to +4 stops,
/// gamma 2.2 and clamped to 8 bits (without rounding), errors averaged over every exposure.
pub fn hdr_psnr(a: &[[f32; 4]], b: &[[f32; 4]]) -> f64 {
    assert_eq!(a.len(), b.len(), "texel counts");
    let tone = |v: f32, scale: f32| (255.0 * (v.max(0.0) * scale).powf(1.0 / 2.2)).min(255.0);
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for stop in -4..=4 {
        let scale = 2f32.powi(stop);
        for (x, y) in a.iter().zip(b) {
            for c in 0..3 {
                let d = (tone(x[c], scale) - tone(y[c], scale)) as f64;
                sum += d * d;
            }
            count += 3;
        }
    }
    let mse = sum / count.max(1) as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}
