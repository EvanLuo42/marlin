//! Scalar `f32` math, including the transcendental functions `core` lacks.
//!
//! `core::f32::math` provides rounding, `sqrt` and `mul_add`, but no `sin`/`exp`/`ln`, which `std`
//! takes from the platform libm. These are written in-house: arguments are reduced and polynomials
//! evaluated in `f64`, then rounded to `f32`. Results are within about 1 ulp of the correctly
//! rounded value and, since only basic IEEE operations are used, bit-identical on every platform
//! (useful for deterministic simulation and replays). Trigonometric functions stay accurate for
//! |x| < ~1.6e6.

// Constants are written as in their fdlibm/FreeBSD sources so they can be checked against them;
// each still parses to the intended f64.
#![allow(clippy::excessive_precision)]

use core::f32::math as fm;
use core::f64::consts as c64;

pub use core::f32::consts::{E, FRAC_PI_2, FRAC_PI_4, LN_2, PI, TAU};

#[inline]
pub fn sqrt(x: f32) -> f32 {
    fm::sqrt(x)
}

#[inline]
pub fn floor(x: f32) -> f32 {
    fm::floor(x)
}

#[inline]
pub fn ceil(x: f32) -> f32 {
    fm::ceil(x)
}

#[inline]
pub fn round(x: f32) -> f32 {
    fm::round(x)
}

#[inline]
pub fn trunc(x: f32) -> f32 {
    fm::trunc(x)
}

/// `x - floor(x)`, in `[0, 1)`.
#[inline]
pub fn fract(x: f32) -> f32 {
    x - fm::floor(x)
}

#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// The `t` for which `lerp(a, b, t) == x`.
#[inline]
pub fn inverse_lerp(a: f32, b: f32, x: f32) -> f32 {
    (x - a) / (b - a)
}

/// Maps `x` from `[a0, a1]` to `[b0, b1]`.
#[inline]
pub fn remap(x: f32, a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    lerp(b0, b1, inverse_lerp(a0, a1, x))
}

/// Clamps to `[0, 1]`.
#[inline]
pub fn saturate(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

/// Hermite step from 0 at `e0` to 1 at `e1`.
#[inline]
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = saturate((x - e0) / (e1 - e0));
    t * t * (3.0 - 2.0 * t)
}

#[inline]
pub fn abs_diff_eq(a: f32, b: f32, epsilon: f32) -> bool {
    (a - b).abs() <= epsilon
}

// --- Trigonometry ---

/// Reduces `x` to `r` in [-π/4, π/4] and the quadrant `n` with `x = n·π/2 + r` (fdlibm's split
/// of π/2: `k · PIO2_HI` is exact for |k| < 2^20).
#[inline]
fn reduce_pio2(x: f32) -> (f64, i64) {
    const PIO2_HI: f64 = 1.570_796_326_734_125_614_17;
    const PIO2_LO: f64 = 6.077_100_506_506_192_249_32e-11;
    let x = x as f64;
    let k = fm_round64(x * c64::FRAC_2_PI);
    (x - k * PIO2_HI - k * PIO2_LO, k as i64)
}

#[inline]
fn fm_round64(x: f64) -> f64 {
    core::f64::math::round(x)
}

/// sin on [-π/4, π/4] (FreeBSD `k_sinf.c`, accurate to f32 precision).
#[inline]
fn kernel_sin(x: f64) -> f64 {
    const S1: f64 = -0.166_666_666_416_265_235_595;
    const S2: f64 = 0.008_333_329_385_889_463_175_6;
    const S3: f64 = -0.000_198_393_348_360_966_317_347;
    const S4: f64 = 0.000_002_718_311_493_989_821_906_4;
    let z = x * x;
    let w = z * z;
    let r = S3 + z * S4;
    let s = z * x;
    (x + s * (S1 + z * S2)) + s * w * r
}

/// cos on [-π/4, π/4] (FreeBSD `k_cosf.c`).
#[inline]
fn kernel_cos(x: f64) -> f64 {
    const C0: f64 = -0.499_999_997_251_031_003_120;
    const C1: f64 = 0.041_666_623_323_739_063_189_4;
    const C2: f64 = -0.001_388_676_377_460_992_946_92;
    const C3: f64 = 0.000_024_390_448_796_277_409_065_4;
    let z = x * x;
    let w = z * z;
    let r = C2 + z * C3;
    ((1.0 + z * C0) + w * C1) + (w * z) * r
}

fn sin_cos64(x: f32) -> (f64, f64) {
    if !x.is_finite() {
        return (f64::NAN, f64::NAN);
    }
    let (r, n) = reduce_pio2(x);
    let (s, c) = (kernel_sin(r), kernel_cos(r));
    match n & 3 {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    }
}

#[inline]
pub fn sin(x: f32) -> f32 {
    sin_cos64(x).0 as f32
}

#[inline]
pub fn cos(x: f32) -> f32 {
    sin_cos64(x).1 as f32
}

/// Both at the cost of one.
#[inline]
pub fn sin_cos(x: f32) -> (f32, f32) {
    let (s, c) = sin_cos64(x);
    (s as f32, c as f32)
}

#[inline]
pub fn tan(x: f32) -> f32 {
    let (s, c) = sin_cos64(x);
    (s / c) as f32
}

/// atan for finite `x` in f64: reduced to |t| <= tan(π/12) with
/// atan(x) = π/6 + atan((x√3 - 1) / (x + √3)), then a Taylor series (error < 2e-10).
fn atan64(x: f64) -> f64 {
    const SQRT3: f64 = 1.732_050_807_568_877_293_53;
    const TAN_PI_12: f64 = 0.267_949_192_431_122_706_47;
    let negative = x < 0.0;
    let mut x = x.abs();
    let mut offset = 0.0;
    if x > 1.0 {
        x = 1.0 / x;
        offset = c64::FRAC_PI_2;
    }
    let mut base = 0.0;
    if x > TAN_PI_12 {
        x = (x * SQRT3 - 1.0) / (x + SQRT3);
        base = c64::FRAC_PI_6;
    }
    let t2 = x * x;
    // x - x³/3 + x⁵/5 - ... + x¹⁵/15
    let mut sum = 0.0;
    let mut k = 15.0;
    while k >= 1.0 {
        sum = 1.0 / k - t2 * sum;
        k -= 2.0;
    }
    let mut r = base + x * sum;
    if offset != 0.0 {
        r = offset - r;
    }
    if negative { -r } else { r }
}

#[inline]
pub fn atan(x: f32) -> f32 {
    if x.is_nan() {
        return x;
    }
    if x.is_infinite() {
        return FRAC_PI_2.copysign(x);
    }
    atan64(x as f64) as f32
}

/// Angle of the point `(x, y)`, in [-π, π], with IEEE handling of zeros and infinities.
pub fn atan2(y: f32, x: f32) -> f32 {
    if x.is_nan() || y.is_nan() {
        return f32::NAN;
    }
    let (ay, ax) = (y.abs() as f64, x.abs() as f64);
    let r = if ay == 0.0 && ax == 0.0 {
        0.0
    } else if ay.is_infinite() && ax.is_infinite() {
        c64::FRAC_PI_4
    } else if ax == 0.0 || ay.is_infinite() {
        c64::FRAC_PI_2
    } else if ax.is_infinite() {
        0.0
    } else {
        atan64(ay / ax)
    };
    let r = if x.is_sign_negative() { c64::PI - r } else { r };
    (r as f32).copysign(y)
}

/// NaN outside [-1, 1].
#[inline]
pub fn asin(x: f32) -> f32 {
    if x == 0.0 {
        return x;
    }
    let x64 = x as f64;
    let c = core::f64::math::sqrt((1.0 - x64) * (1.0 + x64));
    if c.is_nan() {
        f32::NAN
    } else if c == 0.0 {
        FRAC_PI_2.copysign(x)
    } else {
        atan64(x64 / c) as f32
    }
}

/// NaN outside [-1, 1].
#[inline]
pub fn acos(x: f32) -> f32 {
    let x = x as f64;
    let s = core::f64::math::sqrt((1.0 - x) * (1.0 + x));
    let r = if x == 0.0 {
        c64::FRAC_PI_2
    } else {
        let a = atan64(s / x.abs());
        if x < 0.0 { c64::PI - a } else { a }
    };
    if s.is_nan() { f32::NAN } else { r as f32 }
}

// --- Exponentials and logarithms ---

/// e^x for f64 `x` in [-745, 709]: x = k·ln2 + r with |r| <= ln2/2, e^r by Taylor series to
/// degree 12 (error < 1e-14), scaled by 2^k through the exponent bits.
fn exp64(x: f64) -> f64 {
    if x > 709.0 {
        return f64::INFINITY;
    }
    if x < -745.0 {
        return 0.0;
    }
    let k = fm_round64(x * c64::LOG2_E);
    let r = x - k * c64::LN_2;
    let mut sum = 1.0;
    let mut i = 12.0;
    while i >= 1.0 {
        sum = 1.0 + r * sum / i;
        i -= 1.0;
    }
    let k = k as i64;
    // Split the scale so subnormal results stay representable.
    let (k1, k2) = (k / 2, k - k / 2);
    sum * pow2(k1) * pow2(k2)
}

/// 2^k for k in [-1022, 1023].
#[inline]
fn pow2(k: i64) -> f64 {
    f64::from_bits(((k + 1023) as u64) << 52)
}

/// ln of a finite positive f64: x = m·2^e with m in [√½, √2), ln m = 2·atanh(s) with
/// s = (m - 1)/(m + 1), |s| < 0.172, summed to s¹⁵ (error < 1e-12).
fn ln64(x: f64) -> f64 {
    const SQRT2: f64 = c64::SQRT_2;
    let bits = x.to_bits();
    let mut e = ((bits >> 52) & 0x7FF) as i64 - 1023;
    let mut m = f64::from_bits((bits & 0x000F_FFFF_FFFF_FFFF) | (1023 << 52));
    if m > SQRT2 {
        m *= 0.5;
        e += 1;
    }
    let s = (m - 1.0) / (m + 1.0);
    let s2 = s * s;
    let mut sum = 0.0;
    let mut k = 15.0;
    while k >= 1.0 {
        sum = 1.0 / k + s2 * sum;
        k -= 2.0;
    }
    e as f64 * c64::LN_2 + 2.0 * s * sum
}

/// ln for f32 inputs, subnormals included (they are normal as f64).
fn ln_f32(x: f32) -> Option<f64> {
    if x.is_nan() || x < 0.0 {
        return None;
    }
    if x == 0.0 {
        return Some(f64::NEG_INFINITY);
    }
    if x.is_infinite() {
        return Some(f64::INFINITY);
    }
    Some(ln64(x as f64))
}

#[inline]
pub fn exp(x: f32) -> f32 {
    if x.is_nan() {
        return x;
    }
    exp64(x as f64) as f32
}

#[inline]
pub fn exp2(x: f32) -> f32 {
    if x.is_nan() {
        return x;
    }
    exp64(x as f64 * c64::LN_2) as f32
}

/// Natural logarithm: -∞ at 0, NaN below.
#[inline]
pub fn ln(x: f32) -> f32 {
    ln_f32(x).map_or(f32::NAN, |r| r as f32)
}

#[inline]
pub fn log2(x: f32) -> f32 {
    ln_f32(x).map_or(f32::NAN, |r| (r * c64::LOG2_E) as f32)
}

#[inline]
pub fn log10(x: f32) -> f32 {
    ln_f32(x).map_or(f32::NAN, |r| (r * c64::LOG10_E) as f32)
}

/// `x^y` with C `powf` semantics for special values.
pub fn powf(x: f32, y: f32) -> f32 {
    if y == 0.0 || x == 1.0 {
        return 1.0;
    }
    if x.is_nan() || y.is_nan() {
        return f32::NAN;
    }
    let y_int = y == fm::trunc(y);
    let y_odd = y_int && y.abs() < 1.6e7 && (y as i64) % 2 != 0;
    if x == 0.0 {
        let r = if y < 0.0 { f32::INFINITY } else { 0.0 };
        return if y_odd { r.copysign(x) } else { r };
    }
    if x.is_infinite() {
        let r = if y < 0.0 { 0.0 } else { f32::INFINITY };
        return if x < 0.0 && y_odd { -r } else { r };
    }
    if y.is_infinite() {
        let ax = x.abs();
        return if ax == 1.0 {
            1.0
        } else if (ax > 1.0) == (y > 0.0) {
            f32::INFINITY
        } else {
            0.0
        };
    }
    if x < 0.0 && !y_int {
        return f32::NAN;
    }
    let r = exp64(y as f64 * ln64(x.abs() as f64)) as f32;
    if x < 0.0 && y_odd { -r } else { r }
}
