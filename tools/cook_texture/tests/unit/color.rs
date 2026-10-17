//! Color-space and half-float conversions.

use marlin_cook_texture::color::{
    f16_to_f32, f32_to_f16, linear_to_srgb, srgb_to_linear, srgb8_to_linear,
};
use marlin_math::scalar::abs_diff_eq;

#[test]
fn every_half_round_trips() {
    for h in 0..=u16::MAX {
        let v = f16_to_f32(h);
        if v.is_nan() {
            assert!(f16_to_f32(f32_to_f16(v)).is_nan(), "{h:#06x}");
        } else {
            assert_eq!(f32_to_f16(v), h, "{h:#06x} = {v}");
        }
    }
}

#[test]
fn half_rounding() {
    assert_eq!(f32_to_f16(1.0), 0x3c00);
    assert_eq!(f32_to_f16(-2.0), 0xc000);
    assert_eq!(f32_to_f16(65504.0), 0x7bff);
    // Halfway past the largest half rounds to even: infinity.
    assert_eq!(f32_to_f16(65520.0), 0x7c00);
    assert_eq!(f32_to_f16(1e9), 0x7c00);
    assert_eq!(f32_to_f16(f32::NEG_INFINITY), 0xfc00);
    // The smallest subnormal, and half of it (a tie, to even: zero).
    assert_eq!(f32_to_f16(2f32.powi(-24)), 0x0001);
    assert_eq!(f32_to_f16(2f32.powi(-25)), 0x0000);
    assert_eq!(f32_to_f16(1.5 * 2f32.powi(-24)), 0x0002);
    // 1 + 2^-11 is halfway between 1 and the next half: to even (1).
    assert_eq!(f32_to_f16(1.0 + 2f32.powi(-11)), 0x3c00);
    assert_eq!(f32_to_f16(1.0 + 3.0 * 2f32.powi(-11)), 0x3c02);
    // The largest subnormal rounds up into the normals.
    assert_eq!(f32_to_f16(2f32.powi(-14) - 2f32.powi(-25)), 0x0400);
}

#[test]
fn srgb() {
    for i in 0..=255u8 {
        let v = i as f32 / 255.0;
        assert!(
            abs_diff_eq(linear_to_srgb(srgb_to_linear(v)), v, 1e-5),
            "{i}"
        );
        assert_eq!(srgb8_to_linear(i), srgb_to_linear(v));
    }
    assert!(abs_diff_eq(srgb_to_linear(0.5), 0.214_041, 1e-5));
}
