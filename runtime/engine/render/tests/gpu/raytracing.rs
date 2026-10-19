//! The ray tracing scene (R3a): BLASes from the meshes' RT LODs, the TLAS the GPU writes from the
//! GPU Scene, and rays traced against it hitting the instances they point at, where they are; and
//! what they hit (R4a): position, normals and UV0 from the RT LOD, in world space.

use marlin_math::{Quat, Transform, Vec3};
use marlin_render::Environment;
use marlin_render::raytracing::Ray;
use marlin_render::scene::{InstanceId, InstanceUpdate, flags};

use crate::support::Harness;
use crate::support::cook::{sphere, uv_quad};

/// A ray straight down from 10 units above (x, z).
fn down(x: f32, z: f32) -> Ray {
    Ray {
        origin: [x, 10.0, z],
        t_min: 0.0,
        direction: [0.0, -1.0, 0.0],
        t_max: 100.0,
    }
}

/// Unit cubes scaled by `size` centered at the grid's points: (x, 0, z) for x, z in -3..=3
/// step 2.
fn grid(h: &mut Harness, size: f32) -> Vec<(InstanceId, Vec3)> {
    let mut out = Vec::new();
    for x in [-3.0, -1.0, 1.0, 3.0] {
        for z in [-3.0, -1.0, 1.0, 3.0] {
            let at = Vec3::new(x, 0.0, z);
            let id = h.add(
                h.cube,
                Transform {
                    translation: at,
                    rotation: Quat::IDENTITY,
                    scale: Vec3::splat(size),
                },
                [1.0; 4],
            );
            out.push((id, at));
        }
    }
    out
}

/// Frames until every mesh has its BLAS (the default budget builds them all at once).
fn settle(h: &mut Harness, rays: &[Ray]) -> Vec<Option<marlin_render::raytracing::RayHit>> {
    let mut hits = h.trace_frame(rays);
    for _ in 0..4 {
        hits = h.trace_frame(rays);
    }
    hits
}

#[test]
fn rays_hit_the_instances_they_point_at() {
    let mut h = Harness::new(Environment::default());
    let cubes = grid(&mut h, 1.0);
    let mut rays: Vec<Ray> = cubes.iter().map(|(_, at)| down(at.x, at.z)).collect();
    // Between the cubes: misses.
    rays.push(down(0.0, 0.0));
    rays.push(down(2.0, -2.0));
    let hits = settle(&mut h, &rays);
    assert_eq!(h.meshes.blas_count(), 1, "the cube's BLAS");
    for ((id, _), hit) in cubes.iter().zip(&hits) {
        let hit = hit.expect("a cube under the ray");
        assert_eq!(hit.instance, id.index(), "the cube's row");
        // The top face, at y = 0.5: 9.5 below the origin.
        assert!((hit.t - 9.5).abs() < 1e-3, "t {}", hit.t);
        assert!(hit.front_face, "the top face from outside");
    }
    assert!(hits[cubes.len()..].iter().all(Option::is_none), "{hits:?}");
}

#[test]
fn the_tlas_follows_instances_moved_hidden_and_removed() {
    let mut h = Harness::new(Environment::default());
    let cubes = grid(&mut h, 1.0);
    let rays: Vec<Ray> = [(-3.0, -3.0), (5.0, 5.0), (-1.0, -3.0), (-3.0, -1.0)]
        .map(|(x, z)| down(x, z))
        .to_vec();
    settle(&mut h, &rays);
    // The first cube moves to (5, 0, 5), the second hides, the third goes.
    let (moved, _) = cubes[0];
    h.set(
        moved,
        InstanceUpdate::Transform(Transform::from_translation(Vec3::new(5.0, 0.0, 5.0))),
    );
    // Grid order: z varies fastest, so cubes[1] is (-3, -1) and cubes[4] is (-1, -3).
    h.set(cubes[1].0, InstanceUpdate::Flags(flags::HIDDEN));
    h.set(cubes[4].0, InstanceUpdate::Remove);
    let hits = h.trace_frame(&rays);
    assert!(hits[0].is_none(), "the moved cube left (-3, -3)");
    assert_eq!(
        hits[1].map(|h| h.instance),
        Some(moved.index()),
        "and is at (5, 5)"
    );
    assert!(hits[2].is_none(), "the removed cube: {:?}", hits[2]);
    assert!(hits[3].is_none(), "the hidden cube: {:?}", hits[3]);
}

#[test]
fn a_scaled_and_rotated_sphere_is_hit_at_its_surface() {
    let mut h = Harness::new(Environment::default());
    let (ball, _) = h.upload(&sphere(32, 64));
    let id = h.add(
        ball,
        Transform {
            translation: Vec3::new(1.0, 0.0, -2.0),
            rotation: Quat::from_euler_yxz(0.7, 0.3, 0.0),
            scale: Vec3::splat(3.0),
        },
        [1.0; 4],
    );
    // Rays at the center from six directions: radius 1.5.
    let center = Vec3::new(1.0, 0.0, -2.0);
    let rays: Vec<Ray> = [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z]
        .iter()
        .map(|&d| {
            let o = center - d * 10.0;
            Ray {
                origin: [o.x, o.y, o.z],
                t_min: 0.0,
                direction: [d.x, d.y, d.z],
                t_max: 100.0,
            }
        })
        .collect();
    let hits = settle(&mut h, &rays);
    for hit in &hits {
        let hit = hit.expect("the sphere");
        assert_eq!(hit.instance, id.index());
        // The RT LOD is a quarter of the triangles: within a few percent of the radius.
        assert!((hit.t - 8.5).abs() < 0.08, "t {}", hit.t);
    }
}

#[test]
fn blas_builds_keep_to_the_budget() {
    let mut h = Harness::new(Environment::default());
    // The cube's BLAS first, alone.
    h.trace_frame(&[]);
    assert_eq!(h.meshes.blas_count(), 1);
    let triangles = 4 * 32 * 64 / 4;
    h.meshes.set_blas_budget(triangles);
    let balls: Vec<_> = (0..4)
        .map(|i| h.upload(&sphere(32, 64 + 2 * i)).0)
        .collect();
    for (i, &ball) in balls.iter().enumerate() {
        h.add(
            ball,
            Transform::from_translation(Vec3::new(2.0 * i as f32, 0.0, 0.0)),
            [1.0; 4],
        );
    }
    // About one sphere's RT LOD a frame: four more frames to build them all.
    let mut counts = Vec::new();
    for _ in 0..6 {
        h.trace_frame(&[]);
        counts.push(h.meshes.blas_count());
    }
    assert!(counts[0] < 5, "{counts:?}");
    assert!(counts.windows(2).all(|w| w[0] <= w[1]), "{counts:?}");
    assert_eq!(*counts.last().unwrap(), 5, "{counts:?}");
    // And every sphere is traced.
    let rays: Vec<Ray> = (0..4).map(|i| down(2.0 * i as f32, 0.0)).collect();
    let hits = h.trace_frame(&rays);
    assert!(hits.iter().all(Option::is_some), "{hits:?}");
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[test]
fn hits_give_the_surface_position_and_normals() {
    let mut h = Harness::new(Environment::default());
    let (ball, _) = h.upload(&sphere(32, 64));
    let center = Vec3::new(1.0, 0.0, -2.0);
    h.add(
        ball,
        Transform {
            translation: center,
            rotation: Quat::from_euler_yxz(0.7, 0.3, 0.0),
            scale: Vec3::splat(3.0),
        },
        [1.0; 4],
    );
    // Rays at the sphere from a ring of directions, aimed a little off its center.
    let rays: Vec<Ray> = (0..16)
        .map(|i| {
            let a = i as f32 * 0.39;
            let d = Vec3::new(a.cos(), 0.3 * (a * 1.7).sin(), a.sin()).normalize();
            let o = center - d * 10.0 + Vec3::new(0.0, 0.2, 0.0);
            Ray {
                origin: [o.x, o.y, o.z],
                t_min: 0.0,
                direction: [d.x, d.y, d.z],
                t_max: 100.0,
            }
        })
        .collect();
    let hits = settle(&mut h, &rays);
    for (ray, hit) in rays.iter().zip(&hits) {
        let hit = hit.expect("the sphere");
        let p = Vec3::new(hit.position[0], hit.position[1], hit.position[2]);
        // On the ray, at its distance.
        let along = Vec3::from(ray.origin) + Vec3::from(ray.direction) * hit.t;
        assert!((p - along).length() < 1e-3, "{p:?} against {along:?}");
        // On the sphere (radius 1.5), its normals the sphere's, facing the ray.
        let radial = p - center;
        assert!(
            (radial.length() - 1.5).abs() < 0.05,
            "radius {}",
            radial.length()
        );
        let radial = radial.normalize();
        let r = [radial.x, radial.y, radial.z];
        assert!(
            dot(hit.normal, r) > 0.995,
            "normal {:?} at {r:?}",
            hit.normal
        );
        assert!(
            dot(hit.geometric_normal, r) > 0.98,
            "face {:?}",
            hit.geometric_normal
        );
        assert!(dot(hit.normal, ray.direction) < 0.0);
        assert!((dot(hit.normal, hit.normal) - 1.0).abs() < 1e-4);
    }
}

#[test]
fn a_squashed_cube_keeps_its_face_normals() {
    // A non-uniform scale tilts normals that are not transformed by the inverse transpose.
    let mut h = Harness::new(Environment::default());
    let cube = h.cube;
    h.add(
        cube,
        Transform {
            translation: Vec3::ZERO,
            rotation: Quat::from_rotation_z(0.6),
            scale: Vec3::new(4.0, 0.5, 2.0),
        },
        [1.0; 4],
    );
    let hits = settle(&mut h, &[down(0.3, 0.2), down(-0.4, -0.5)]);
    let up = [-(0.6f32).sin(), (0.6f32).cos(), 0.0];
    for hit in &hits {
        let hit = hit.expect("the cube");
        assert!(
            dot(hit.normal, up) > 0.9999,
            "{:?} against {up:?}",
            hit.normal
        );
        assert!(dot(hit.geometric_normal, up) > 0.9999);
    }
}

#[test]
fn hits_interpolate_uv0() {
    let mut h = Harness::new(Environment::default());
    let (quad, _) = h.upload(&uv_quad());
    // Lying face up, 4 units wide: UV = (x / 4 + 0.5, 0.5 − z / 4).
    h.add(
        quad,
        Transform {
            translation: Vec3::ZERO,
            rotation: Quat::from_rotation_x(-core::f32::consts::FRAC_PI_2),
            scale: Vec3::splat(4.0),
        },
        [1.0; 4],
    );
    let points = [(0.0, 0.0), (1.0, 0.5), (-1.5, 1.2), (1.9, -1.9)];
    let rays: Vec<Ray> = points.iter().map(|&(x, z)| down(x, z)).collect();
    let hits = settle(&mut h, &rays);
    for (&(x, z), hit) in points.iter().zip(&hits) {
        let hit = hit.expect("the quad");
        let want = [x / 4.0 + 0.5, 0.5 - z / 4.0];
        // Half precision: a thousandth.
        assert!(
            (hit.uv[0] - want[0]).abs() < 2e-3 && (hit.uv[1] - want[1]).abs() < 2e-3,
            "at ({x}, {z}): {:?} against {want:?}",
            hit.uv
        );
        assert!(dot(hit.normal, [0.0, 1.0, 0.0]) > 0.9999);
    }
}
