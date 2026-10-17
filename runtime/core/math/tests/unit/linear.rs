//! Vectors, quaternions, matrices and transforms, including the engine's conventions.

use marlin_math::scalar::{FRAC_PI_2, abs_diff_eq};
use marlin_math::*;
use marlin_test_support::scale;

const EPS: f32 = 1e-5;

fn vec3(rng: &mut Rng) -> Vec3 {
    Vec3::new(
        rng.range_f32(-10.0, 10.0),
        rng.range_f32(-10.0, 10.0),
        rng.range_f32(-10.0, 10.0),
    )
}

#[test]
fn layout_matches_gpu_expectations() {
    use core::mem::{align_of, size_of};
    assert_eq!((size_of::<Vec2>(), align_of::<Vec2>()), (8, 4));
    assert_eq!((size_of::<Vec3>(), align_of::<Vec3>()), (12, 4));
    assert_eq!((size_of::<Vec4>(), align_of::<Vec4>()), (16, 16));
    assert_eq!((size_of::<Quat>(), align_of::<Quat>()), (16, 16));
    assert_eq!((size_of::<Mat4>(), align_of::<Mat4>()), (64, 16));
    assert_eq!(size_of::<Mat3>(), 36);
}

#[test]
fn right_handed_y_up() {
    assert_eq!(Vec3::X.cross(Vec3::Y), Vec3::Z);
    assert_eq!(Vec3::RIGHT.cross(Vec3::UP), Vec3::BACK);
    assert_eq!(Vec3::FORWARD, Vec3::NEG_Z);
    // Counter-clockwise about +Y seen from above: +X turns toward -Z (forward).
    let q = Quat::from_rotation_y(FRAC_PI_2);
    assert!((q * Vec3::X).abs_diff_eq(Vec3::NEG_Z, EPS));
    assert!((Quat::from_rotation_x(FRAC_PI_2) * Vec3::Y).abs_diff_eq(Vec3::Z, EPS));
    assert!((Quat::from_rotation_z(FRAC_PI_2) * Vec3::X).abs_diff_eq(Vec3::Y, EPS));
}

#[test]
fn vector_ops() {
    let a = Vec3::new(1.0, 2.0, 3.0);
    let b = Vec3::new(4.0, -5.0, 6.0);
    assert_eq!(a + b, Vec3::new(5.0, -3.0, 9.0));
    assert_eq!(a * 2.0, 2.0 * a);
    assert_eq!(a.dot(b), 12.0);
    assert_eq!(-a, Vec3::new(-1.0, -2.0, -3.0));
    assert_eq!(a[2], 3.0);
    assert!(abs_diff_eq(a.normalize().length(), 1.0, EPS));
    assert_eq!(Vec3::ZERO.try_normalize(), None);
    assert_eq!(Vec3::new(3.0, 0.0, 4.0).length(), 5.0);
    assert!(
        Vec3::new(1.0, -1.0, 0.0)
            .reflect(Vec3::Y)
            .abs_diff_eq(Vec3::new(1.0, 1.0, 0.0), EPS)
    );
    assert!(abs_diff_eq(Vec3::X.angle_between(Vec3::Y), FRAC_PI_2, EPS));

    let mut rng = Rng::new(42);
    for _ in 0..scale(1000) {
        let n = rng.unit_vec3();
        let o = n.any_orthonormal_vector();
        assert!(o.is_normalized() && abs_diff_eq(n.dot(o), 0.0, EPS));
    }

    let mut v = Vec4::new(1.0, 2.0, 3.0, 4.0);
    v.y = 7.0;
    assert_eq!((v.x, v.y, v.w), (1.0, 7.0, 4.0));
    assert_eq!(v.dot(Vec4::ONE), 15.0);
    assert_eq!(v.max_element(), 7.0);
    assert_eq!(
        Vec4::new(1.0, -2.0, 3.0, -4.0).abs(),
        Vec4::new(1.0, 2.0, 3.0, 4.0)
    );
    assert_eq!(IVec2::new(3, 4) * 2 - IVec2::ONE, IVec2::new(5, 7));
    assert!(abs_diff_eq(Vec2::from_angle(0.5).to_angle(), 0.5, EPS));
}

#[test]
fn quaternion_algebra() {
    let mut rng = Rng::new(7);
    for _ in 0..scale(1000) {
        let (a, b, v) = (rng.rotation(), rng.rotation(), vec3(&mut rng));
        // Composition matches applying in sequence, and the matrix form agrees.
        assert!(((a * b) * v).abs_diff_eq(a * (b * v), 1e-3));
        assert!((Mat3::from_quat(a) * v).abs_diff_eq(a * v, 1e-3));
        assert!(Quat::from_mat3(&Mat3::from_quat(a)).rotation_eq(a, 1e-4));
        assert!((a.conjugate() * (a * v)).abs_diff_eq(v, 1e-3));
        assert!((a * a.inverse()).rotation_eq(Quat::IDENTITY, 1e-5));

        let (axis, angle) = a.to_axis_angle();
        assert!(Quat::from_axis_angle(axis, angle).rotation_eq(a, 1e-4));

        let (from, to) = (rng.unit_vec3(), rng.unit_vec3());
        assert!((Quat::from_rotation_arc(from, to) * from).abs_diff_eq(to, 1e-4));
    }
    // Opposite vectors take the fallback path.
    assert!(
        (Quat::from_rotation_arc(Vec3::X, Vec3::NEG_X) * Vec3::X).abs_diff_eq(Vec3::NEG_X, EPS)
    );
}

#[test]
fn euler_yxz_round_trip() {
    let mut rng = Rng::new(11);
    for _ in 0..scale(1000) {
        let (yaw, pitch, roll) = (
            rng.range_f32(-3.0, 3.0),
            rng.range_f32(-1.5, 1.5),
            rng.range_f32(-3.0, 3.0),
        );
        let q = Quat::from_euler_yxz(yaw, pitch, roll);
        let (y2, p2, r2) = q.to_euler_yxz();
        assert!(Quat::from_euler_yxz(y2, p2, r2).rotation_eq(q, 1e-4));
    }
    // Gimbal lock still reproduces the rotation.
    let q = Quat::from_euler_yxz(0.7, FRAC_PI_2, 0.3);
    let (y, p, r) = q.to_euler_yxz();
    assert_eq!(r, 0.0);
    assert!(Quat::from_euler_yxz(y, p, r).rotation_eq(q, 1e-3));
}

#[test]
fn slerp_is_constant_speed_on_the_short_path() {
    let a = Quat::IDENTITY;
    let b = Quat::from_rotation_y(2.0);
    assert!(a.slerp(b, 0.0).rotation_eq(a, EPS));
    assert!(a.slerp(b, 1.0).rotation_eq(b, EPS));
    for t in [0.25, 0.5, 0.75] {
        assert!(abs_diff_eq(a.slerp(b, t).angle_between(a), 2.0 * t, 1e-4));
    }
    // -b is the same rotation; slerp must not take the long way round.
    assert!(abs_diff_eq(a.slerp(-b, 0.5).angle_between(a), 1.0, 1e-4));
}

/// Determinant of an n×n column-major matrix by cofactor expansion along the first row.
fn det_f64(m: &[f64], n: usize) -> f64 {
    if n == 1 {
        return m[0];
    }
    (0..n)
        .map(|col| {
            let minor: Vec<f64> = (0..n)
                .filter(|&c| c != col)
                .flat_map(|c| (1..n).map(move |r| m[c * n + r]))
                .collect();
            let sign = if col % 2 == 0 { 1.0 } else { -1.0 };
            sign * m[col * n] * det_f64(&minor, n - 1)
        })
        .sum()
}

#[test]
fn matrix_inverse_and_determinant() {
    let mut rng = Rng::new(3);
    for _ in 0..scale(1000) {
        let m = Mat4::from_cols_array(&core::array::from_fn(|_| rng.range_f32(-5.0, 5.0)));
        if m.determinant().abs() < 1e-2 {
            continue;
        }
        assert!(
            (m * m.inverse()).abs_diff_eq(&Mat4::IDENTITY, 1e-3),
            "{m:?}"
        );
        // Against an f64 Laplace expansion, with a tolerance relative to the Hadamard bound
        // (product of column lengths), the scale f32 rounding works at.
        let cols = m.to_cols_array().map(f64::from);
        let hadamard: f64 = (0..4)
            .map(|c| (0..4).map(|r| cols[c * 4 + r].powi(2)).sum::<f64>().sqrt())
            .product();
        let error = (m.determinant() as f64 - det_f64(&cols, 4)).abs();
        assert!(
            error <= 1e-5 * hadamard,
            "det error {error} vs bound {hadamard}"
        );
        assert!(m.transpose().transpose().abs_diff_eq(&m, 0.0));

        let m3 = Mat3::from_mat4(&m);
        if m3.determinant().abs() > 1e-2 {
            assert!((m3 * m3.inverse()).abs_diff_eq(&Mat3::IDENTITY, 1e-3));
        }
    }
}

#[test]
fn trs_compose_and_decompose() {
    let mut rng = Rng::new(5);
    for _ in 0..scale(1000) {
        let s = Vec3::new(
            rng.range_f32(0.1, 4.0),
            rng.range_f32(0.1, 4.0),
            rng.range_f32(0.1, 4.0),
        );
        let (r, t, p) = (rng.rotation(), vec3(&mut rng), vec3(&mut rng));
        let m = Mat4::from_scale_rotation_translation(s, r, t);
        // Scale first, then rotate, then translate.
        assert!(m.transform_point3(p).abs_diff_eq(r * (p * s) + t, 1e-3));
        assert!((m * m.inverse_affine()).abs_diff_eq(&Mat4::IDENTITY, 1e-4));

        let (s2, r2, t2) = m.to_scale_rotation_translation();
        assert!(s2.abs_diff_eq(s, 1e-4) && t2.abs_diff_eq(t, 1e-4) && r2.rotation_eq(r, 1e-3));

        let tr = Transform {
            translation: t,
            rotation: r,
            scale: Vec3::splat(s.x),
        };
        assert!(tr.to_matrix().transform_point3(p).abs_diff_eq(tr * p, 1e-3));
        assert!((tr.inverse() * (tr * p)).abs_diff_eq(p, 1e-3));
        let child = Transform::from_translation(vec3(&mut rng));
        assert!(((tr * child) * p).abs_diff_eq(tr * (child * p), 1e-3));
    }
}

#[test]
fn view_matrix_looks_down_negative_z() {
    let eye = Vec3::new(3.0, 2.0, 5.0);
    let target = Vec3::new(-1.0, 0.5, -2.0);
    let view = Mat4::look_at_rh(eye, target, Vec3::UP);
    assert!(view.transform_point3(eye).abs_diff_eq(Vec3::ZERO, 1e-5));
    let d = target.distance(eye);
    assert!(
        view.transform_point3(target)
            .abs_diff_eq(Vec3::new(0.0, 0.0, -d), 1e-4)
    );
    // The camera's right stays horizontal.
    assert!(view.transform_vector3(Vec3::UP).y > 0.0);

    let cam = Transform::from_translation(eye).looking_at(target, Vec3::UP);
    assert!(cam.forward().abs_diff_eq((target - eye).normalize(), 1e-5));
}

#[test]
fn projections_are_reverse_z() {
    let (near, far) = (0.1, 1000.0);
    let p = Mat4::perspective_rh(1.0, 16.0 / 9.0, near, far);
    assert!(abs_diff_eq(
        p.project_point3(Vec3::new(0.0, 0.0, -near)).z,
        1.0,
        1e-5
    ));
    assert!(abs_diff_eq(
        p.project_point3(Vec3::new(0.0, 0.0, -far)).z,
        0.0,
        1e-5
    ));
    let mid = p.project_point3(Vec3::new(0.0, 0.0, -10.0)).z;
    assert!(
        mid > 0.0 && mid < 0.02,
        "reverse-Z keeps far depths near 0: {mid}"
    );
    // The top edge of the field of view lands on NDC y = 1.
    let top = Vec3::new(0.0, (0.5f32).tan() * 5.0, -5.0);
    assert!(abs_diff_eq(p.project_point3(top).y, 1.0, 1e-4));

    let inf = Mat4::perspective_infinite_rh(1.0, 1.0, near);
    assert!(abs_diff_eq(
        inf.project_point3(Vec3::new(0.0, 0.0, -near)).z,
        1.0,
        1e-5
    ));
    assert!(inf.project_point3(Vec3::new(0.0, 0.0, -1e7)).z < 1e-7);

    let o = Mat4::orthographic_rh(-2.0, 2.0, -1.0, 1.0, near, far);
    assert!(
        (o.project_point3(Vec3::new(2.0, 1.0, -near)) - Vec3::new(1.0, 1.0, 1.0)).length() < 1e-5
    );
    assert!(
        (o.project_point3(Vec3::new(-2.0, -1.0, -far)) - Vec3::new(-1.0, -1.0, 0.0)).length()
            < 1e-4
    );
}
