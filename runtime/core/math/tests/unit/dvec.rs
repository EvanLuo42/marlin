//! Double-precision positions: precision far from the origin, and agreement with `f32` math.

use marlin_math::*;

#[test]
fn relative_positions_keep_f32_precision_far_away() {
    // 10 000 km out, an f32 position's step is a full meter; relative to a nearby origin the
    // offset is exact to f32 rounding of the small difference.
    let origin = DVec3::new(1.0e7, -2.5e6, 7.0e6);
    let p = origin + DVec3::new(0.125, 3.0e-4, -42.75);
    let r = p.relative_to(origin);
    assert!((r - Vec3::new(0.125, 3.0e-4, -42.75)).abs().max_element() < 1e-6);
    let f32_step = (origin.x as f32).next_up() - origin.x as f32;
    assert_eq!(f32_step, 1.0);
}

#[test]
fn rotation_agrees_with_f32() {
    let mut rng = Rng::new(7);
    for _ in 0..1000 {
        let axis = Vec3::new(
            rng.range_f32(-1.0, 1.0),
            rng.range_f32(-1.0, 1.0),
            rng.range_f32(-1.0, 1.0),
        )
        .normalize_or_zero();
        if axis == Vec3::ZERO {
            continue;
        }
        let q = Quat::from_axis_angle(axis, rng.range_f32(-3.0, 3.0));
        let v = Vec3::new(
            rng.range_f32(-10.0, 10.0),
            rng.range_f32(-10.0, 10.0),
            rng.range_f32(-10.0, 10.0),
        );
        let wide = q * DVec3::from_vec3(v);
        assert!((wide.as_vec3() - q * v).abs().max_element() < 1e-4);
    }
}

#[test]
fn large_offsets_rotate_without_losing_meters() {
    // A 90° turn about Y of a point 5000 km away lands exactly where it should.
    let q = Quat::from_axis_angle(Vec3::Y, core::f32::consts::FRAC_PI_2);
    let p = q * DVec3::new(5.0e6, 0.0, 0.0);
    // The quaternion itself is f32, so the error scales with the distance times f32 epsilon.
    assert!(p.abs_diff_eq(DVec3::new(0.0, 0.0, -5.0e6), 5.0e6 * 1e-6));
}

#[test]
fn arithmetic() {
    let a = DVec3::new(1.0, 2.0, 3.0);
    let b = DVec3::new(-4.0, 0.5, 2.0);
    assert_eq!(a + b, DVec3::new(-3.0, 2.5, 5.0));
    assert_eq!(a - b, DVec3::new(5.0, 1.5, 1.0));
    assert_eq!(a * 2.0, DVec3::new(2.0, 4.0, 6.0));
    assert_eq!(-a, DVec3::new(-1.0, -2.0, -3.0));
    assert_eq!(a.dot(b), -4.0 + 1.0 + 6.0);
    assert_eq!(DVec3::new(3.0, 4.0, 0.0).length(), 5.0);
    assert_eq!(a + Vec3::new(1.0, 1.0, 1.0), DVec3::new(2.0, 3.0, 4.0));
    assert_eq!(a.scale(Vec3::new(2.0, 0.5, 1.0)), DVec3::new(2.0, 1.0, 3.0));
    assert_eq!(a.lerp(b, 0.5), DVec3::new(-1.5, 1.25, 2.5));
    assert_eq!(a[2], 3.0);
}
