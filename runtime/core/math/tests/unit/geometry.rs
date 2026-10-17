//! Bounding volumes, rays and frustum culling.

use marlin_math::scalar::abs_diff_eq;
use marlin_math::*;

#[test]
fn aabb_basics() {
    let b = Aabb::from_points([Vec3::new(1.0, -2.0, 3.0), Vec3::new(-1.0, 4.0, 0.0)]);
    assert_eq!(
        b,
        Aabb::new(Vec3::new(-1.0, -2.0, 0.0), Vec3::new(1.0, 4.0, 3.0))
    );
    assert!(Aabb::EMPTY.is_empty() && !b.is_empty());
    assert!(b.contains_point(Vec3::ZERO + Vec3::Y));
    assert!(!b.contains_point(Vec3::new(0.0, 5.0, 0.0)));
    assert!(b.intersects(&Aabb::new(Vec3::splat(0.5), Vec3::splat(9.0))));
    assert!(!b.intersects(&Aabb::new(Vec3::splat(5.0), Vec3::splat(9.0))));
}

#[test]
fn transformed_aabb_contains_transformed_corners() {
    let b = Aabb::new(Vec3::new(-1.0, -2.0, -0.5), Vec3::new(3.0, 1.0, 2.0));
    let m = Mat4::from_scale_rotation_translation(
        Vec3::new(2.0, 0.5, 1.5),
        Quat::from_euler_yxz(0.7, -0.4, 1.1),
        Vec3::new(10.0, -3.0, 4.0),
    );
    let t = b.transformed(&m);
    for i in 0..8 {
        let corner = Vec3::new(
            if i & 1 == 0 { b.min.x } else { b.max.x },
            if i & 2 == 0 { b.min.y } else { b.max.y },
            if i & 4 == 0 { b.min.z } else { b.max.z },
        );
        let p = m.transform_point3(corner);
        assert!(
            t.grow(p) == t || t.min.abs_diff_eq(t.grow(p).min, 1e-4),
            "corner {i} outside"
        );
    }
}

#[test]
fn rays() {
    let b = Aabb::new(Vec3::splat(-1.0), Vec3::splat(1.0));
    let r = Ray::new(Vec3::new(-5.0, 0.0, 0.0), Vec3::X);
    assert_eq!(r.intersect_aabb(&b), Some((4.0, 6.0)));
    assert_eq!(
        Ray::new(Vec3::new(5.0, 0.0, 0.0), Vec3::X).intersect_aabb(&b),
        None
    );
    let (enter, exit) = Ray::new(Vec3::ZERO, Vec3::Y).intersect_aabb(&b).unwrap();
    assert!(enter < 0.0 && exit == 1.0);
    // Axis-parallel rays outside a slab miss.
    assert_eq!(
        Ray::new(Vec3::new(-5.0, 2.0, 0.0), Vec3::X).intersect_aabb(&b),
        None
    );

    let ground = Plane::from_point_normal(Vec3::ZERO, Vec3::UP);
    let down = Ray::new(Vec3::new(1.0, 10.0, 2.0), Vec3::DOWN);
    assert_eq!(down.intersect_plane(&ground), Some(10.0));
    assert_eq!(Ray::new(Vec3::Y, Vec3::X).intersect_plane(&ground), None);

    let s = Sphere::new(Vec3::new(0.0, 0.0, -10.0), 2.0);
    let fwd = Ray::new(Vec3::ZERO, Vec3::FORWARD);
    assert!(abs_diff_eq(fwd.intersect_sphere(&s).unwrap(), 8.0, 1e-5));
    assert_eq!(Ray::new(Vec3::ZERO, Vec3::BACK).intersect_sphere(&s), None);
    assert_eq!(Ray::new(s.center, Vec3::X).intersect_sphere(&s), Some(0.0));
}

fn camera() -> Mat4 {
    let view = Mat4::look_at_rh(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO, Vec3::UP);
    Mat4::perspective_rh(1.2, 1.0, 0.5, 100.0) * view
}

#[test]
fn frustum_culling() {
    let f = Frustum::from_view_projection(&camera());
    assert!(f.contains_point(Vec3::ZERO));
    assert!(
        !f.contains_point(Vec3::new(0.0, 0.0, 20.0)),
        "behind the camera"
    );
    assert!(
        !f.contains_point(Vec3::new(0.0, 0.0, 9.8)),
        "before the near plane"
    );
    assert!(
        !f.contains_point(Vec3::new(0.0, 0.0, -95.0)),
        "past the far plane"
    );
    assert!(
        !f.contains_point(Vec3::new(50.0, 0.0, 0.0)),
        "off to the right"
    );

    assert!(f.intersects_sphere(&Sphere::new(Vec3::new(9.0, 0.0, 0.0), 3.0)));
    assert!(!f.intersects_sphere(&Sphere::new(Vec3::new(30.0, 0.0, 0.0), 3.0)));
    assert!(f.intersects_aabb(&Aabb::new(Vec3::splat(-1.0), Vec3::splat(1.0))));
    assert!(!f.intersects_aabb(&Aabb::new(
        Vec3::new(40.0, -1.0, -1.0),
        Vec3::new(42.0, 1.0, 1.0)
    )));
    assert!(!f.intersects_aabb(&Aabb::new(
        Vec3::new(-1.0, -1.0, 15.0),
        Vec3::new(1.0, 1.0, 17.0)
    )));

    // Every plane faces inward: the frustum's own center is in front of all of them.
    for plane in &f.planes {
        assert!(plane.signed_distance(Vec3::new(0.0, 0.0, -10.0)) > 0.0);
        assert!(abs_diff_eq(plane.normal.length(), 1.0, 1e-5));
    }
}

#[test]
fn infinite_frustum_has_no_far_plane() {
    let view = Mat4::look_at_rh(Vec3::ZERO, Vec3::FORWARD, Vec3::UP);
    let f = Frustum::from_view_projection(&(Mat4::perspective_infinite_rh(1.0, 1.0, 0.1) * view));
    assert!(f.contains_point(Vec3::new(0.0, 0.0, -1e6)));
    assert!(!f.contains_point(Vec3::new(0.0, 0.0, 1.0)));
}
