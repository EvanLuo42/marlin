//! The cooked mesh and texture encodings.

use marlin_render::asset::mesh::{
    basis, decode_tangent_frame, dequantize_position, encode_tangent_frame, f16_to_f32, f32_to_f16,
    position_scale, quantize_position,
};
use marlin_render::asset::texture::{PixelFormat, TextureError, TextureView};
use marlin_serial::cooked::CookedBuffer;

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    v.map(|c| c / l)
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[test]
fn basis_is_orthonormal() {
    let mut state = 1u64;
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    };
    for _ in 0..10_000 {
        let n = normalize([next(), next(), next()]);
        let (a, b) = basis(n);
        for (x, y) in [(a, b), (a, n), (b, n)] {
            assert!(dot(x, y).abs() < 1e-5);
        }
        assert!((dot(a, a) - 1.0).abs() < 1e-5 && (dot(b, b) - 1.0).abs() < 1e-5);
        assert!(dot(cross(a, b), n) > 0.99, "right-handed");
    }
}

#[test]
fn tangent_frames_round_trip_within_a_fraction_of_a_degree() {
    let mut state = 7u64;
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    };
    let mut worst_normal = 1.0f32;
    let mut worst_tangent = 1.0f32;
    for i in 0..20_000 {
        let n = normalize([next(), next(), next()]);
        let r = normalize([next(), next(), next()]);
        let t = normalize(cross(n, r));
        let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
        let (dn, dt, ds) = decode_tangent_frame(encode_tangent_frame(n, t, sign));
        assert_eq!(ds, sign);
        worst_normal = worst_normal.min(dot(n, dn));
        worst_tangent = worst_tangent.min(dot(t, dt));
        assert!(dot(dn, dt).abs() < 1e-4, "decoded frame is orthogonal");
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
            assert!((d[i] - p[i]).abs() <= scale[i] * 0.5 + 1e-6);
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
