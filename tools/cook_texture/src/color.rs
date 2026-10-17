//! Color-space and half-float conversions.

use std::sync::OnceLock;

pub use marlin_math::scalar::{f16_to_f32, f32_to_f16};

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
