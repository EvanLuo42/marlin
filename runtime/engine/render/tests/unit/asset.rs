//! The cooked mesh and texture encodings.

use marlin_math::scalar::abs_diff_eq;
use marlin_math::{Rng, Vec3};
use marlin_render::asset::mesh::{
    basis, decode_tangent_frame, dequantize_position, encode_tangent_frame, f16_to_f32, f32_to_f16,
    position_scale, quantize_position,
};
use marlin_render::asset::texture::{PixelFormat, TextureError, TextureView};
use marlin_serial::cooked::CookedBuffer;

#[test]
fn basis_is_orthonormal() {
    let mut rng = Rng::new(1);
    let mut next = || rng.range_f32(-1.0, 1.0);
    for _ in 0..10_000 {
        let n = Vec3::new(next(), next(), next()).normalize();
        let (a, b) = basis(n.to_array());
        let (a, b) = (Vec3::from(a), Vec3::from(b));
        for (x, y) in [(a, b), (a, n), (b, n)] {
            assert!(abs_diff_eq(x.dot(y), 0.0, 1e-5));
        }
        assert!(abs_diff_eq(a.length_squared(), 1.0, 1e-5));
        assert!(abs_diff_eq(b.length_squared(), 1.0, 1e-5));
        assert!(a.cross(b).dot(n) > 0.99, "right-handed");
    }
}

#[test]
fn tangent_frames_round_trip_within_a_fraction_of_a_degree() {
    let mut rng = Rng::new(7);
    let mut next = || rng.range_f32(-1.0, 1.0);
    let mut worst_normal = 1.0f32;
    let mut worst_tangent = 1.0f32;
    for i in 0..20_000 {
        let n = Vec3::new(next(), next(), next()).normalize();
        let r = Vec3::new(next(), next(), next()).normalize();
        let t = n.cross(r).normalize();
        let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
        let (dn, dt, ds) =
            decode_tangent_frame(encode_tangent_frame(n.to_array(), t.to_array(), sign));
        let (dn, dt) = (Vec3::from(dn), Vec3::from(dt));
        assert_eq!(ds, sign);
        worst_normal = worst_normal.min(n.dot(dn));
        worst_tangent = worst_tangent.min(t.dot(dt));
        assert!(
            abs_diff_eq(dn.dot(dt), 0.0, 1e-4),
            "decoded frame is orthogonal"
        );
    }
    // 10-bit octahedral normals: ~0.2°; 11-bit tangent angle plus the normal's error: ~0.4°.
    assert!(
        worst_normal.acos().to_degrees() < 0.25,
        "normal {}°",
        worst_normal.acos().to_degrees()
    );
    assert!(
        worst_tangent.acos().to_degrees() < 0.5,
        "tangent {}°",
        worst_tangent.acos().to_degrees()
    );
}

#[test]
fn halves_round_trip() {
    for bits in 0..=u16::MAX {
        let f = f16_to_f32(bits);
        if f.is_nan() {
            assert!(f16_to_f32(f32_to_f16(f)).is_nan());
        } else {
            assert_eq!(f32_to_f16(f), bits, "{bits:#06x}");
        }
    }
    assert_eq!(f32_to_f16(1.0 + 1.0 / 4096.0), 0x3c00, "ties to even");
    assert_eq!(f32_to_f16(65520.0), 0x7c00, "overflow to infinity");
    assert_eq!(f32_to_f16(1e-9), 0);
}

#[test]
fn positions_quantize_over_their_bounds() {
    let (min, max) = ([-2.0, 0.0, 5.0], [2.0, 1.0, 5.0]);
    let scale = position_scale(min, max);
    for p in [[-2.0, 0.0, 5.0], [2.0, 1.0, 5.0], [0.123, 0.456, 5.0]] {
        let q = quantize_position(p, min, scale);
        let d = dequantize_position(q, min, scale);
        for i in 0..3 {
            assert!(abs_diff_eq(d[i], p[i], scale[i] * 0.5 + 1e-6), "{p:?}");
        }
    }
}

#[test]
fn a_damaged_texture_payload_is_rejected() {
    assert!(matches!(
        TextureView::new(&[0u8; 8], &[]),
        Err(TextureError::Layout(_))
    ));
    // A header claiming an unknown format.
    let mut payload = vec![0u8; 48];
    payload[0..4].copy_from_slice(&99u32.to_le_bytes());
    let aligned = CookedBuffer::copy_from(&payload, 16);
    assert_eq!(
        TextureView::new(aligned.as_slice(), &[]).unwrap_err(),
        TextureError::UnknownFormat(99)
    );
    assert_eq!(
        PixelFormat::from_code(PixelFormat::Bc7UnormSrgb as u32),
        Some(PixelFormat::Bc7UnormSrgb)
    );
}
