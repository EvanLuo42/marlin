//! The visibility pipeline: instance culling, the DAG traversal and LOD cut, cluster culling,
//! the mesh shader's 64-bit visibility buffer and the depth split out of it, checked against
//! the same cut computed on the CPU and drawn in the same frame by a reference rasterizer.

use std::collections::{HashMap, HashSet};

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

/// Checks the frame's visibility buffer against the reference: the clusters drawn are clusters
/// of the CPU's cut, each once; the same pixels covered, at the same depth (the two pipelines
/// rasterize the same positions: on the test GPU they agree exactly); the depth texture split
/// out exactly; every id naming a drawn cluster and one of its triangles.
pub(crate) fn check_against_reference(h: &Harness, f: &VisFrame) {
    let [_, found, drawn] = f.counters;
    assert_eq!(found, drawn, "the capacity held every cluster");
    assert!(f.hw_drawn <= f.capacity && f.sw_drawn <= f.capacity);
    let cut: HashSet<[u32; 2]> = f.cut.iter().copied().collect();
    let mut seen = HashSet::new();
    let drawn: HashSet<u32> = f.drawn().iter().map(|&(id, _)| id).collect();
    for (_, c) in f.drawn() {
        assert!(cut.contains(&c), "{c:x?} is not in the cut");
        assert!(seen.insert(c), "{c:x?} drawn twice");
    }
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
        let cluster = cluster_of(v);
        assert!(drawn.contains(&(cluster as u32)), "pixel {p}: {v:x}");
        let record = h.cluster(f.clusters[cluster][1]);
        assert!(
            (v & 127) < u64::from(record.triangle_count),
            "pixel {p}: {v:x}"
        );
        // The software raster interpolates depth in floating point, the hardware its own way:
        // within a few parts in 10 000 (coverage is the same).
        let d = (f.depth[p] - f.vis_depth[p]).abs();
        assert!(
            d <= 1e-6 || d <= f.depth[p] * 5e-4,
            "pixel {p}: depths {} and {}",
            f.depth[p],
            f.vis_depth[p]
        );
    }
}

/// Frames until streaming has caught up, each checked against the reference; the last (the
/// first frame that drew every page it wanted).
pub(crate) fn settle(h: &mut Harness) -> VisFrame {
    for _ in 0..64 {
        let f = h.visibility_frame();
        check_against_reference(h, &f);
        if h.meshes.streaming_idle() {
            return f;
        }
    }
    panic!("streaming did not settle in 64 frames");
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

/// Four instances in view (two overlapping, one scaled unevenly) and two out of it.
fn scene(h: &mut Harness) -> (Vec<marlin_render::scene::InstanceId>, HashMap<u32, u32>) {
    let (ball, _) = h.upload(&sphere(16, 32));
    let colors = [
        [1.0, 0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
        [1.0, 1.0, 0.0, 1.0],
    ];
    let inside = vec![
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
    let rows = colors
        .iter()
        .zip(&inside)
        .map(|(c, id)| (rgba8(*c), id.index()))
        .collect();
    (inside, rows)
}

#[test]
fn the_visibility_buffer_matches_the_reference() {
    let mut h = Harness::new(Environment::default());
    h.camera_at(4.0);
    let (inside, rows) = scene(&mut h);
    let f = settle(&mut h);
    assert_eq!(f.counters[0], 4, "the instances in the frustum");
    assert!(f.levels >= 1);
    check_against_reference(&h, &f);
    check_instances(&f, &rows);

    // Moving an instance out of view drops it; the next frame agrees again.
    h.set(
        inside[1],
        InstanceUpdate::Transform(Transform::from_translation(Vec3::new(0.0, 50.0, 0.0))),
    );
    let f = settle(&mut h);
    assert_eq!(f.counters[0], 3);
    check_against_reference(&h, &f);
    check_instances(&f, &rows);
}

#[test]
fn coarser_cuts_still_match_the_reference() {
    let mut h = Harness::new(Environment::default());
    h.camera_at(4.0);
    let (_, rows) = scene(&mut h);
    for budget in [0.5, 1.0, 4.0, 16.0] {
        h.vis.lod_error_pixels = budget;
        let f = settle(&mut h);
        check_against_reference(&h, &f);
        check_instances(&f, &rows);
    }
}

#[test]
fn hidden_removed_and_unresident_instances_are_not_drawn() {
    let mut h = Harness::new(Environment::default());
    h.camera_at(3.0);
    let a = h.add(h.cube, Transform::IDENTITY, [1.0, 0.0, 0.0, 1.0]);
    let f = settle(&mut h);
    assert_eq!(f.counters[0], 1);
    check_against_reference(&h, &f);

    h.set(a, InstanceUpdate::Flags(flags::HIDDEN));
    let f = settle(&mut h);
    assert_eq!(f.counters[0], 0, "hidden");
    assert!(f.visbuffer.iter().all(|&v| v == 0));
    check_against_reference(&h, &f);
    h.set(a, InstanceUpdate::Flags(0));
    assert_eq!(settle(&mut h).counters[0], 1, "shown again");

    // A mesh referenced but not resident yet draws nothing; once resident it does.
    let pending = h.scene.ids().meshes.reserve();
    h.updates.meshes.push(pending, MeshUpdate::Add);
    h.set(a, InstanceUpdate::Mesh(pending));
    assert_eq!(settle(&mut h).counters[0], 0, "not resident");
    let (_, handle) = h.upload(&cube());
    let resident = h.meshes.resident_mesh(handle).unwrap();
    h.updates
        .meshes
        .push(pending, MeshUpdate::Resident(resident));
    let f = settle(&mut h);
    assert_eq!(f.counters[0], 1, "resident");
    check_against_reference(&h, &f);

    // Removed: nothing, and its row comes back for the next instance.
    h.set(a, InstanceUpdate::Remove);
    let f = settle(&mut h);
    assert_eq!(f.counters, [0, 0, 0]);
    let b = h.add(h.cube, Transform::IDENTITY, [0.0, 1.0, 0.0, 1.0]);
    assert_eq!(b.index(), a.index());
    let f = settle(&mut h);
    assert_eq!(f.counters[0], 1);
    check_against_reference(&h, &f);
}

#[test]
fn back_facing_clusters_are_culled_without_changing_the_picture() {
    let mut h = Harness::new(Environment::default());
    let s = sphere(32, 64);
    let (ball, _) = h.upload(&s);
    h.camera_at(2.0);
    h.add(ball, Transform::IDENTITY, [1.0; 4]);
    let f = settle(&mut h);
    assert_eq!(f.counters[0], 1);
    let drawn = f.counters[2];
    // A budget of 0 draws the finest level: a curved surface's every simplification has an
    // error.
    let finest = f.cut.len() as u32;
    assert!(f.cut.iter().all(|c| h.cluster(c[1]).level == 0));
    assert!(
        drawn > 0 && drawn < finest * 3 / 4,
        "{drawn} of {finest} clusters"
    );
    check_against_reference(&h, &f);
}

#[test]
fn farther_instances_draw_coarser_cuts() {
    let mut h = Harness::new(Environment::default());
    h.vis.lod_error_pixels = 1.0;
    let (ball, _) = h.upload(&sphere(64, 128));
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
    let f = settle(&mut h);
    assert_eq!(f.counters[0], ids.len() as u32);
    check_against_reference(&h, &f);
    // Triangles and the finest level in each instance's cut.
    let mut triangles: HashMap<u32, u32> = HashMap::new();
    let mut finest: HashMap<u32, u8> = HashMap::new();
    for c in &f.cut {
        let r = h.cluster(c[1]);
        *triangles.entry(c[0]).or_default() += u32::from(r.triangle_count);
        let l = finest.entry(c[0]).or_insert(u8::MAX);
        *l = (*l).min(r.level);
    }
    let by_depth: Vec<u32> = ids.iter().map(|id| triangles[&id.index()]).collect();
    assert!(
        by_depth.windows(2).all(|w| w[0] >= w[1]),
        "triangles by distance: {by_depth:?}"
    );
    // The farthest draws the roots: about a coarsest cluster per root group.
    assert!(by_depth[0] > by_depth[4] * 4, "{by_depth:?}");
    assert!(finest[&ids[4].index()] > 0, "the farthest is coarse");
}

#[test]
fn an_empty_view_leaves_the_buffer_clear() {
    let mut h = Harness::new(Environment::default());
    h.camera_at(3.0);
    let f = settle(&mut h);
    assert_eq!(f.counters, [0, 0, 0]);
    assert_eq!((f.nodes, f.parts), (0, 0));
    assert!(f.visbuffer.iter().all(|&v| v == 0));
    assert!(f.vis_depth.iter().all(|&d| d == 0.0));
}
