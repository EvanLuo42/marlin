//! Numbers the host has no type for: IEEE half precision, converted in software so every host
//! gives the same bits (0006 §11).

/// The `f32` that the `f16` with bits `h` stands for (exact).
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = u32::from(h >> 15) << 31;
    let exp = u32::from((h >> 10) & 0x1f);
    let mant = u32::from(h & 0x3ff);
    let bits = match (exp, mant) {
        (0, 0) => sign,
        // Subnormal: normalize.
        (0, _) => {
            let shift = mant.leading_zeros() - 21;
            let mant = (mant << shift) & 0x3ff;
            let exp = 127 - 15 + 1 - shift;
            sign | (exp << 23) | (mant << 13)
        }
        (0x1f, 0) => sign | 0x7f80_0000,
        // NaN: keep the payload's top bits and make it quiet.
        (0x1f, _) => sign | 0x7fc0_0000 | (mant << 13),
        _ => sign | ((exp + 127 - 15) << 23) | (mant << 13),
    };
    f32::from_bits(bits)
}

/// `x` rounded to the nearest `f16`, ties to even; overflow gives infinity.
pub fn f32_to_f16(x: f32) -> u16 {
    let bits = x.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let mant = bits & 0x7f_ffff;
    if exp == 0xff {
        return if mant == 0 {
            sign | 0x7c00
        } else {
            sign | 0x7e00 | (mant >> 13) as u16
        };
    }
    // The value is mant' * 2^(e - 23) with the implicit bit; rebase it on f16's exponent.
    let unbiased = exp - 127;
    if unbiased > 15 {
        return sign | 0x7c00;
    }
    let (full, shift) = if unbiased >= -14 {
        // Normal in f16: keep 10 mantissa bits.
        (mant | 0x80_0000, 13)
    } else {
        // Subnormal (or zero) in f16.
        if exp == 0 {
            return sign;
        }
        let shift = 13 + (-14 - unbiased);
        if shift > 24 {
            return sign;
        }
        (mant | 0x80_0000, shift as u32)
    };
    let kept = full >> shift;
    let rest = full & ((1 << shift) - 1);
    let half = 1 << (shift - 1);
    let mut rounded = kept;
    if rest > half || (rest == half && kept & 1 == 1) {
        rounded += 1;
    }
    if unbiased >= -14 {
        // `rounded` carries the implicit bit at 1 << 10; a carry out bumps the exponent.
        let e = (unbiased + 15) as u32;
        let value = (e << 10) + (rounded - 0x400);
        if value >= 0x7c00 {
            sign | 0x7c00
        } else {
            sign | value as u16
        }
    } else {
        // A carry into 1 << 10 becomes the smallest normal, which the encoding gives for free.
        sign | rounded as u16
    }
}

/// `x` rounded to the nearest `f16`, ties to even (once, not through `f32`).
pub fn f64_to_f16(x: f64) -> u16 {
    let bits = x.to_bits();
    let sign = ((bits >> 48) & 0x8000) as u16;
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let mant = bits & ((1 << 52) - 1);
    if exp == 0x7ff {
        return if mant == 0 {
            sign | 0x7c00
        } else {
            sign | 0x7e00 | (mant >> 42) as u16
        };
    }
    let unbiased = exp - 1023;
    if unbiased > 15 {
        return sign | 0x7c00;
    }
    if exp == 0 {
        return sign;
    }
    let full = mant | (1 << 52);
    let shift = if unbiased >= -14 {
        42
    } else {
        let shift = 42 + (-14 - unbiased) as u32;
        if shift > 53 {
            return sign;
        }
        shift
    };
    let kept = full >> shift;
    let rest = full & ((1u64 << shift) - 1);
    let half = 1u64 << (shift - 1);
    let mut rounded = kept;
    if rest > half || (rest == half && kept & 1 == 1) {
        rounded += 1;
    }
    if unbiased >= -14 {
        let e = (unbiased + 15) as u64;
        let value = (e << 10) + (rounded - 0x400);
        if value >= 0x7c00 {
            sign | 0x7c00
        } else {
            sign | value as u16
        }
    } else {
        sign | rounded as u16
    }
}
