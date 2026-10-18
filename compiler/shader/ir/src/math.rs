//! Math functions for the interpreter (0006 §11): computed in `f64` with IEEE basic operations
//! only (`+ - * /`, `sqrt`, comparisons, bit manipulation), never the platform's libm, so every
//! host gives the same bits and cooking on any machine gives the same blobs. Accuracy is a few
//! `f64` ulps, far inside GPU tolerances once rounded to `f32` or `f16`; it is not correctly
//! rounded.

use std::f64::consts::{FRAC_PI_2, FRAC_PI_6, LN_2, PI};

/// `x * 2^n`, exact unless the result is subnormal.
pub fn ldexp(mut x: f64, mut n: i32) -> f64 {
    let p1023 = f64::from_bits(0x7fe0_0000_0000_0000); // 2^1023
    let m1022 = f64::from_bits(0x0010_0000_0000_0000); // 2^-1022
    while n > 1023 {
        x *= p1023;
        n -= 1023;
        if x.is_infinite() {
            return x;
        }
    }
    while n < -1022 {
        x *= m1022;
        n += 1022;
        if x == 0.0 {
            return x;
        }
    }
    x * f64::from_bits(((n + 1023) as u64) << 52)
}

/// The exact remainder `x - n * y` with `n` the quotient truncated toward zero (C's `fmod`).
pub fn fmod(x: f64, y: f64) -> f64 {
    let (mut ux, mut uy) = (x.to_bits(), y.to_bits());
    let mut ex = ((ux >> 52) & 0x7ff) as i64;
    let mut ey = ((uy >> 52) & 0x7ff) as i64;
    let sx = ux >> 63;
    if uy << 1 == 0 || y.is_nan() || ex == 0x7ff {
        return f64::NAN;
    }
    if ux << 1 <= uy << 1 {
        if ux << 1 == uy << 1 {
            return 0.0 * x;
        }
        return x;
    }
    if ex == 0 {
        let mut i = ux << 12;
        while i >> 63 == 0 {
            ex -= 1;
            i <<= 1;
        }
        ux <<= (-ex + 1) as u32;
    } else {
        ux &= u64::MAX >> 12;
        ux |= 1 << 52;
    }
    if ey == 0 {
        let mut i = uy << 12;
        while i >> 63 == 0 {
            ey -= 1;
            i <<= 1;
        }
        uy <<= (-ey + 1) as u32;
    } else {
        uy &= u64::MAX >> 12;
        uy |= 1 << 52;
    }
    while ex > ey {
        let i = ux.wrapping_sub(uy);
        if i >> 63 == 0 {
            if i == 0 {
                return 0.0 * x;
            }
            ux = i;
        }
        ux <<= 1;
        ex -= 1;
    }
    let i = ux.wrapping_sub(uy);
    if i >> 63 == 0 {
        if i == 0 {
            return 0.0 * x;
        }
        ux = i;
    }
    while ux >> 52 == 0 {
        ux <<= 1;
        ex -= 1;
    }
    if ex > 0 {
        ux -= 1 << 52;
        ux |= (ex as u64) << 52;
    } else {
        ux >>= (-ex + 1) as u32;
    }
    f64::from_bits(ux | (sx << 63))
}

/// `e^r` for `|r| <= 0.35`, by Taylor series.
fn exp_small(r: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    for k in 1..=15 {
        term *= r / k as f64;
        sum += term;
    }
    sum
}

// ln 2 split so that `n * LN2_HI` is exact for |n| < 2^11 (fdlibm's values).
const LN2_HI: f64 = f64::from_bits(0x3fe6_2e42_fee0_0000);
const LN2_LO: f64 = f64::from_bits(0x3dea_39ef_3579_3c76);

pub fn exp(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x > 709.8 {
        return f64::INFINITY;
    }
    if x < -745.2 {
        return 0.0;
    }
    let n = (x / LN_2).round_ties_even();
    let r = (x - n * LN2_HI) - n * LN2_LO;
    ldexp(exp_small(r), n as i32)
}

pub fn exp2(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x >= 1024.0 {
        return f64::INFINITY;
    }
    if x < -1075.0 {
        return 0.0;
    }
    let n = x.round_ties_even();
    let f = x - n;
    ldexp(exp_small(f * LN_2), n as i32)
}

/// `(e, ln m)` with `x = m * 2^e` and `m` in `[sqrt(1/2), sqrt(2))`, for finite `x > 0`.
fn ln_parts(x: f64) -> (f64, f64) {
    let mut bits = x.to_bits();
    let mut e = ((bits >> 52) & 0x7ff) as i64;
    if e == 0 {
        // Subnormal: scale up by 2^54.
        bits = (x * f64::from_bits(0x4350_0000_0000_0000)).to_bits();
        e = ((bits >> 52) & 0x7ff) as i64 - 54;
    }
    let mut m = f64::from_bits((bits & ((1 << 52) - 1)) | (1023 << 52));
    let mut e = e - 1023;
    if m > std::f64::consts::SQRT_2 {
        m *= 0.5;
        e += 1;
    }
    // ln m = 2 atanh(s) = 2 (s + s^3/3 + s^5/5 + ...), |s| <= 0.172.
    let s = (m - 1.0) / (m + 1.0);
    let s2 = s * s;
    let mut sum = 0.0;
    for k in (0..14).rev() {
        sum = sum * s2 + 1.0 / (2 * k + 1) as f64;
    }
    (e as f64, 2.0 * s * sum)
}

fn log_special(x: f64) -> Option<f64> {
    if x.is_nan() || x < 0.0 {
        Some(f64::NAN)
    } else if x == 0.0 {
        Some(f64::NEG_INFINITY)
    } else if x.is_infinite() {
        Some(f64::INFINITY)
    } else {
        None
    }
}

pub fn ln(x: f64) -> f64 {
    if let Some(s) = log_special(x) {
        return s;
    }
    let (e, l) = ln_parts(x);
    e * LN2_HI + (e * LN2_LO + l)
}

pub fn log2(x: f64) -> f64 {
    if let Some(s) = log_special(x) {
        return s;
    }
    let (e, l) = ln_parts(x);
    e + l / LN_2
}

/// `x^y`, as GPUs define it: `exp2(y * log2(x))`, with IEEE's special cases; a negative base
/// takes only integer exponents.
pub fn pow(x: f64, y: f64) -> f64 {
    if y == 0.0 || x == 1.0 {
        return 1.0;
    }
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    let y_int = y == y.trunc();
    let y_odd = y_int && fmod(y, 2.0).abs() == 1.0;
    if x == 0.0 {
        let zero = if y > 0.0 { 0.0 } else { f64::INFINITY };
        return if y_odd && x.is_sign_negative() {
            -zero
        } else {
            zero
        };
    }
    if x < 0.0 {
        if !y_int {
            return f64::NAN;
        }
        let r = pow(-x, y);
        return if y_odd { -r } else { r };
    }
    if x.is_infinite() {
        return if y > 0.0 { f64::INFINITY } else { 0.0 };
    }
    if y.is_infinite() {
        return match (x > 1.0, y > 0.0) {
            (true, true) | (false, false) => f64::INFINITY,
            _ => 0.0,
        };
    }
    exp2(y * log2(x))
}

// pi/2 split into three parts for argument reduction (fdlibm's values).
const PIO2_1: f64 = f64::from_bits(0x3ff9_21fb_5440_0000);
const PIO2_2: f64 = f64::from_bits(0x3dd0_b461_1a60_0000);
const PIO2_3: f64 = f64::from_bits(0x3ba3_198a_2e00_0000);

/// `x` as `k * pi/2 + r` with `|r| <= ~pi/4`: `(k mod 4, r)`.
fn reduce(x: f64) -> (i64, f64) {
    // Bring huge arguments into range first; this reduction is deterministic, not exact.
    let x = if x.abs() > 1e6 { fmod(x, 2.0 * PI) } else { x };
    let k = (x / FRAC_PI_2).round_ties_even();
    let r = ((x - k * PIO2_1) - k * PIO2_2) - k * PIO2_3;
    ((k as i64).rem_euclid(4), r)
}

fn sin_small(r: f64) -> f64 {
    let r2 = r * r;
    let mut term = r;
    let mut total = r;
    for j in 1..=9 {
        term *= -r2 / ((2 * j) as f64 * (2 * j + 1) as f64);
        total += term;
    }
    total
}

fn cos_small(r: f64) -> f64 {
    let r2 = r * r;
    let mut term = 1.0;
    let mut total = 1.0;
    for j in 1..=9 {
        term *= -r2 / ((2 * j - 1) as f64 * (2 * j) as f64);
        total += term;
    }
    total
}

pub fn sin(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    if x == 0.0 {
        return x;
    }
    let (k, r) = reduce(x);
    match k {
        0 => sin_small(r),
        1 => cos_small(r),
        2 => -sin_small(r),
        _ => -cos_small(r),
    }
}

pub fn cos(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let (k, r) = reduce(x);
    match k {
        0 => cos_small(r),
        1 => -sin_small(r),
        2 => -cos_small(r),
        _ => sin_small(r),
    }
}

pub fn tan(x: f64) -> f64 {
    sin(x) / cos(x)
}

/// atan for `0 <= t <= 1`.
fn atan_unit(t: f64) -> f64 {
    const TAN_PI_12: f64 = 0.267_949_192_431_122_7; // 2 - sqrt(3)
    const SQRT_3: f64 = 1.732_050_807_568_877_2;
    let (base, t) = if t > TAN_PI_12 {
        (FRAC_PI_6, (t * SQRT_3 - 1.0) / (SQRT_3 + t))
    } else {
        (0.0, t)
    };
    base + t * sum_series(t * t)
}

/// 1 - u/3 + u^2/5 - u^3/7 + ...
fn sum_series(u: f64) -> f64 {
    let mut sum = 0.0;
    for k in (0..22).rev() {
        let term = 1.0 / (2 * k + 1) as f64;
        let signed = if k % 2 == 0 { term } else { -term };
        sum = signed + sum * u;
    }
    sum
}

pub fn atan(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    let a = x.abs();
    let r = if a > 1.0 {
        FRAC_PI_2 - atan_unit(1.0 / a)
    } else {
        atan_unit(a)
    };
    if x < 0.0 { -r } else { r }
}

pub fn atan2(y: f64, x: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    if y == 0.0 {
        return if x.is_sign_negative() {
            PI.copysign(y)
        } else {
            y
        };
    }
    if x == 0.0 {
        return FRAC_PI_2.copysign(y);
    }
    if x.is_infinite() || y.is_infinite() {
        let (ax, ay) = (x.is_infinite(), y.is_infinite());
        let r = match (ax, ay, x > 0.0) {
            (true, true, true) => PI / 4.0,
            (true, true, false) => 3.0 * PI / 4.0,
            (true, false, true) => 0.0,
            (true, false, false) => PI,
            _ => FRAC_PI_2,
        };
        return r.copysign(y);
    }
    let r = atan(y / x);
    if x > 0.0 {
        r
    } else if y > 0.0 {
        r + PI
    } else {
        r - PI
    }
}

pub fn asin(x: f64) -> f64 {
    if !(-1.0..=1.0).contains(&x) {
        return f64::NAN;
    }
    atan2(x, ((1.0 - x) * (1.0 + x)).sqrt())
}

pub fn acos(x: f64) -> f64 {
    if !(-1.0..=1.0).contains(&x) {
        return f64::NAN;
    }
    atan2(((1.0 - x) * (1.0 + x)).sqrt(), x)
}

pub fn sinh(x: f64) -> f64 {
    if x.abs() < 1.0 {
        let x2 = x * x;
        let mut term = x;
        let mut total = x;
        for j in 1..=11 {
            term *= x2 / ((2 * j) as f64 * (2 * j + 1) as f64);
            total += term;
        }
        return total;
    }
    if x.abs() > 710.5 {
        return f64::INFINITY.copysign(x);
    }
    let e = exp(x.abs());
    let r = 0.5 * (e - 1.0 / e);
    r.copysign(x)
}

pub fn cosh(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x.abs() > 710.5 {
        return f64::INFINITY;
    }
    let e = exp(x.abs());
    0.5 * (e + 1.0 / e)
}

pub fn tanh(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x.abs() > 22.0 {
        return 1.0f64.copysign(x);
    }
    sinh(x) / cosh(x)
}
