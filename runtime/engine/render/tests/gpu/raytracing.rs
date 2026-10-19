//! The ray tracing scene (R3a): BLASes from the meshes' RT LODs, the TLAS the GPU writes from the
//! GPU Scene, and rays traced against it hitting the instances they point at, where they are.

use marlin_math::{Quat, Transform, Vec3};
use marlin_render::Environment;
use marlin_render::raytracing::Ray;
use marlin_render::scene::{InstanceId, InstanceUpdate, flags};

use crate::support::Harness;
use crate::support::cook::sphere;

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
