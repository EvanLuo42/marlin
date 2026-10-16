//! Color-space and half-float conversions.

use std::sync::OnceLock;

/// sRGB-encoded value to linear light (IEC 61966-2-1).
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light to an sRGB-encoded value.
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// [`srgb_to_linear`] of an 8-bit value, from a table.
pub fn srgb8_to_linear(c: u8) -> f32 {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| core::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0)))[c as usize]
}

/// IEEE 754 binary16 bits of `v`, rounded to nearest even. Overflow gives infinity; NaN stays NaN.
pub fn f32_to_f16(v: f32) -> u16 {
    let x = v.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xff) as i32;
    let mant = x & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if mant != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        // Subnormal: the implicit bit joins the mantissa, shifted to the 2^-24 unit.
        let m = mant | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = m >> shift;
        let rem = m & ((1 << shift) - 1);
        let mid = 1 << (shift - 1);
        let up = rem > mid || (rem == mid && half & 1 == 1);
        return sign | (half + up as u32) as u16;
    }
    let half = ((e as u32) << 10) | (mant >> 13);
    let rem = mant & 0x1fff;
    // A carry out of the mantissa correctly bumps the exponent (up to infinity).
    let up = rem > 0x1000 || (rem == 0x1000 && half & 1 == 1);
    sign | (half + up as u32) as u16
}

/// The value of binary16 bits `h`.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h as u32) & 0x8000) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let mant = (h & 0x3ff) as u32;
    let bits = match exp {
        0 => {
            let v = mant as f32 * (1.0 / 16_777_216.0);
            return if sign != 0 { -v } else { v };
        }
        31 => sign | 0x7f80_0000 | (mant << 13),
        _ => sign | ((exp + 112) << 23) | (mant << 13),
    };
    f32::from_bits(bits)
}
