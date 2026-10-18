//! The visibility pipeline (R1j): instance and cluster culling, the mesh shader's 64-bit
//! visibility buffer and the depth split out of it, checked against a reference rasterizer
//! drawing the same scene in the same frame (each instance's LOD 0 as an indexed draw).

use std::collections::HashMap;

use marlin_math::{Quat, Transform, Vec3};
use marlin_render::Environment;
use marlin_render::scene::{InstanceUpdate, MeshUpdate, flags};

use crate::support::cook::{cube, sphere};
use crate::support::{Harness, SIZE, VisFrame};

fn cluster_of(v: u64) -> usize {
    ((v >> 7) & ((1 << 25) - 1)) as usize
}

/// The 8-bit RGBA word the reference writes for `color`.
fn rgba8(c: [f32; 4]) -> u32 {
    let b = c.map(|x| (x * 255.0).round() as u32);
    b[0] | b[1] << 8 | b[2] << 16 | b[3] << 24
}

/// Checks the frame's visibility buffer against the reference: the same pixels covered, at the
/// same depth (the two pipelines rasterize the same positions: on the test GPU they agree
/// exactly), the depth texture split out exactly, and every id naming a drawn cluster and a
/// triangle of a meshlet.
fn check_against_reference(f: &VisFrame) {
    let [_, found, drawn] = f.counters;
    assert_eq!(found, drawn, "the capacity held every cluster");
    assert!(drawn <= f.capacity);
    for p in 0..(SIZE * SIZE) as usize {
        let v = f.visbuffer[p];
        assert_eq!(
            f.vis_depth[p],
            f32::from_bits((v >> 32) as u32),
            "pixel {p}"
        );
        assert_eq!(f.depth[p] > 0.0, v != 0, "pixel {p}: covered by one only");
        if v == 0 {
            continue;
        }
        assert!(cluster_of(v) < drawn as usize, "pixel {p}: {v:x}");
        assert!((v & 127) < 124, "pixel {p}: {v:x}");
        assert!(
            (f.depth[p] - f.vis_depth[p]).abs() <= 1e-6,
            "pixel {p}: depths"
        );
    }
}

/// Checks that every covered pixel's cluster belongs to the instance whose color the reference
/// wrote there, `rows` mapping the colors to instance rows.
fn check_instances(f: &VisFrame, rows: &HashMap<u32, u32>) {
    for p in 0..(SIZE * SIZE) as usize {
        let v = f.visbuffer[p];
        if v == 0 {
            continue;
        }
        let row = f.clusters[cluster_of(v)][0];
        assert_eq!(rows.get(&f.color[p]), Some(&row), "pixel {p}");
    }
}

#[test]
fn the_visibility_buffer_matches_the_reference() {
    let mut h = Harness::new(Environment::default());
    let (ball, _) = h.upload(&sphere(16, 32));
    h.camera_at(4.0);
    let colors = [
        [1.0, 0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
        [1.0, 1.0, 0.0, 1.0],
    ];
    let inside = [
        h.add(
            h.cube,
            Transform {
                translation: Vec3::new(-1.0, 0.5, 0.0),
                rotation: Quat::from_euler_yxz(0.6, 0.3, 0.0),
                scale: Vec3::ONE,
            },
            colors[0],
        ),
        h.add(
            h.cube,
            Transform {
                translation: Vec3::new(1.0, -0.4, -1.0),
                rotation: Quat::from_rotation_z(0.4),
                scale: Vec3::splat(1.5),
            },
            colors[1],
        ),
        // Overlaps the first: depth decides.
        h.add(
            ball,
            Transform::from_translation(Vec3::new(-0.6, 0.2, 0.4)),
            colors[2],
        ),
        // Non-uniform scale: no cone culling, scaled bounds.
        h.add(
            ball,
            Transform {
                translation: Vec3::new(0.6, 0.9, 0.5),
                rotation: Quat::from_rotation_y(0.8),
                scale: Vec3::new(1.6, 0.6, 0.9),
            },
            colors[3],
        ),
    ];
    // Behind the camera and far off to the side: culled.
    h.add(
        h.cube,
        Transform::from_translation(Vec3::new(0.0, 0.0, 8.0)),
        [1.0; 4],
    );
    h.add(
        h.cube,
        Transform::from_translation(Vec3::new(40.0, 0.0, 0.0)),
        [1.0; 4],
    );
    let rows: HashMap<u32, u32> = colors
        .iter()
        .zip(&inside)
        .map(|(c, id)| (rgba8(*c), id.index()))
        .collect();
    let f = h.visibility_frame();
    assert_eq!(f.counters[0], 4, "the instances in the frustum");
    check_against_reference(&f);
    check_instances(&f, &rows);

    // Moving an instance out of view drops it; the next frame agrees again.
    h.set(
        inside[1],
        InstanceUpdate::Transform(Transform::from_translation(Vec3::new(0.0, 50.0, 0.0))),
    );
    let f = h.visibility_frame();
    assert_eq!(f.counters[0], 3);
    check_against_reference(&f);
    check_instances(&f, &rows);
}

#[test]
fn hidden_removed_and_unresident_instances_are_not_drawn() {
    let mut h = Harness::new(Environment::default());
    h.camera_at(3.0);
    let a = h.add(h.cube, Transform::IDENTITY, [1.0, 0.0, 0.0, 1.0]);
    let f = h.visibility_frame();
    assert_eq!(f.counters[0], 1);
    check_against_reference(&f);

    h.set(a, InstanceUpdate::Flags(flags::HIDDEN));
    let f = h.visibility_frame();
    assert_eq!(f.counters[0], 0, "hidden");
    assert!(f.visbuffer.iter().all(|&v| v == 0));
    check_against_reference(&f);
    h.set(a, InstanceUpdate::Flags(0));
    assert_eq!(h.visibility_frame().counters[0], 1, "shown again");

    // A mesh referenced but not resident yet draws nothing; once resident it does.
    let pending = h.scene.ids().meshes.reserve();
    h.updates.meshes.push(pending, MeshUpdate::Add);
    h.set(a, InstanceUpdate::Mesh(pending));
    assert_eq!(h.visibility_frame().counters[0], 0, "not resident");
    let data = cube();
    let handle = h
        .meshes
        .upload_mesh(h.device, &data.view(), &data.pages())
        .unwrap();
    let resident = h.meshes.resident_mesh(handle).unwrap();
    h.updates
        .meshes
        .push(pending, MeshUpdate::Resident(resident));
    let f = h.visibility_frame();
    assert_eq!(f.counters[0], 1, "resident");
    check_against_reference(&f);

    // Removed: nothing, and its row comes back for the next instance.
    h.set(a, InstanceUpdate::Remove);
    let f = h.visibility_frame();
    assert_eq!(f.counters, [0, 0, 0]);
    let b = h.add(h.cube, Transform::IDENTITY, [0.0, 1.0, 0.0, 1.0]);
    assert_eq!(b.index(), a.index());
    let f = h.visibility_frame();
    assert_eq!(f.counters[0], 1);
    check_against_reference(&f);
}

#[test]
fn back_facing_clusters_are_culled_without_changing_the_picture() {
    let mut h = Harness::new(Environment::default());
    let s = sphere(32, 64);
    let meshlets = s.view().lods[0].meshlet_count;
    let (ball, _) = h.upload(&s);
    h.camera_at(2.0);
    h.add(ball, Transform::IDENTITY, [1.0; 4]);
    let f = h.visibility_frame();
    assert_eq!(f.counters[0], 1);
    let drawn = f.counters[2];
    assert!(
        drawn > 0 && drawn < meshlets * 3 / 4,
        "{drawn} of {meshlets} meshlets"
    );
    // Every drawn cluster is LOD 0 of the ball's row.
    for c in &f.clusters[..drawn as usize] {
        assert_eq!(c[1] >> 24, 0);
        assert!((c[1] & 0xFF_FFFF) < meshlets);
    }
    check_against_reference(&f);
}

#[test]
fn instances_draw_the_coarsest_lod_their_error_budget_allows() {
    let mut h = Harness::new(Environment::default());
    let budget = 1.0;
    h.vis.lod_error_pixels = budget;
    let s = sphere(32, 64);
    let view = s.view();
    let errors: Vec<f32> = view.lods.iter().map(|l| l.error).collect();
    assert!(errors.len() >= 3);
    let radius = view.header.sphere[3];
    let (ball, _) = h.upload(&s);
    h.camera_at(0.0);
    let depths = [-1.5f32, -4.0, -12.0, -40.0, -300.0];
    let ids: Vec<_> = depths
        .iter()
        .map(|&z| {
            h.add(
                ball,
                Transform::from_translation(Vec3::new(0.0, 0.0, z)),
                [1.0; 4],
            )
        })
        .collect();
    let f = h.visibility_frame();
    assert_eq!(f.counters[0], ids.len() as u32);
    let mut lods: HashMap<u32, u32> = HashMap::new();
    for c in &f.clusters[..f.counters[2] as usize] {
        let lod = c[1] >> 24;
        assert_eq!(
            *lods.entry(c[0]).or_insert(lod),
            lod,
            "one LOD per instance"
        );
    }
    // The coarsest LOD whose error, projected at the sphere's near side, is within the budget
    // (`Marlin.Engine.Visibility.InstanceCull`'s formula, on the CPU).
    let pixels_per_unit = SIZE as f32 / (2.0 * (core::f32::consts::FRAC_PI_3 * 0.5).tan());
    for (id, z) in ids.iter().zip(depths) {
        let distance = (-z - radius).max(1e-4);
        let limit = budget * distance / pixels_per_unit;
        let expected = errors.iter().rposition(|&e| e <= limit).unwrap_or(0) as u32;
        assert_eq!(lods.get(&id.index()), Some(&expected), "at {z}");
    }
    assert!(lods.values().any(|&l| l > 0), "{lods:?}");
}

#[test]
fn an_empty_view_leaves_the_buffer_clear() {
    let mut h = Harness::new(Environment::default());
    h.camera_at(3.0);
    let f = h.visibility_frame();
    assert_eq!(f.counters, [0, 0, 0]);
    assert!(f.visbuffer.iter().all(|&v| v == 0));
    assert!(f.vis_depth.iter().all(|&d| d == 0.0));
}
