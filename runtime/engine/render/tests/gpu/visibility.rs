//! The visibility pipeline (R1j): instance and cluster culling, the mesh shader's 64-bit
//! visibility buffer and the depth split out of it, checked against the forward pass drawing the
//! same scene in the same frame.

use std::collections::HashMap;

use marlin_gpu_test_support::shader;
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::{Environment, Visibility, VisibilityShaders};

use crate::support::cook::sphere;
use crate::support::{Harness, SIZE, VisFrame};

/// Lit straight on with no ambient and a black background, so faces facing the camera come out
/// in exactly their color.
fn flat_environment() -> Environment {
    Environment {
        light_direction: -Vec3::Z,
        light_color: [1.0; 3],
        ambient: [0.0; 3],
        clear_color: [0.0, 0.0, 0.0, 1.0],
    }
}

/// The engine's visibility pipeline, for the binary's life (graph passes borrow it for any
/// frame), with `lod_error_pixels` as given.
fn visibility(h: &Harness, lod_error_pixels: f32) -> &'static Visibility {
    let entry = |name: &str| shader(&format!("Marlin.Engine.Visibility.{name}"));
    let vis = Box::leak(Box::new(
        Visibility::new(
            h.device,
            &VisibilityShaders {
                reset: entry("Reset"),
                instance_cull: entry("InstanceCull"),
                instance_args: entry("InstanceArgs"),
                cluster_cull: entry("ClusterCull"),
                finish_args: entry("FinishArgs"),
                mesh: entry("VisMesh"),
                pixel: entry("VisPs"),
                clear: entry("VisClear"),
                depth: entry("VisDepth"),
            },
        )
        .unwrap(),
    ));
    vis.lod_error_pixels = lod_error_pixels;
    vis
}

fn cluster_of(v: u64) -> usize {
    ((v >> 7) & ((1 << 25) - 1)) as usize
}

/// Checks the frame's visibility buffer against its forward pass: the same pixels covered and
/// the same depth (but for `edge_pixels`; the two pipelines rasterize the same positions, so on
/// the test GPU they agree exactly), the depth texture split out exactly, and every id naming a
/// drawn cluster and a triangle of a meshlet.
fn check_against_forward(f: &VisFrame, edge_pixels: usize) {
    let [_, found, drawn] = f.counters;
    assert_eq!(found, drawn, "the capacity held every cluster");
    assert!(drawn <= f.capacity);
    let mut coverage = 0;
    let mut depth = 0;
    for p in 0..(SIZE * SIZE) as usize {
        let v = f.visbuffer[p];
        assert_eq!(
            f.vis_depth[p],
            f32::from_bits((v >> 32) as u32),
            "pixel {p}"
        );
        let (forward, visible) = (f.depth[p] > 0.0, v != 0);
        if forward != visible {
            coverage += 1;
            continue;
        }
        if !visible {
            continue;
        }
        assert!(cluster_of(v) < drawn as usize, "pixel {p}: {v:x}");
        assert!((v & 127) < 124, "pixel {p}: {v:x}");
        if (f.depth[p] - f.vis_depth[p]).abs() > 1e-6 {
            depth += 1;
        }
    }
    assert!(
        coverage <= edge_pixels,
        "{coverage} pixels covered by one but not the other"
    );
    assert!(depth <= edge_pixels, "{depth} pixels at different depths");
}

#[test]
fn the_visibility_buffer_matches_the_forward_pass() {
    let mut h = Harness::new(flat_environment());
    let vis = visibility(&h, 0.0);
    h.renderer.lod_error_pixels = 0.0;
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
    let f = h.visibility_frame(vis);
    assert_eq!(f.counters[0], 4, "the instances in the frustum");
    check_against_forward(&f, 0);

    // Every pixel's cluster belongs to the instance whose color the forward pass wrote there.
    let rows: HashMap<u32, u32> = colors
        .iter()
        .zip(&inside)
        .map(|(c, id)| {
            let rgba = c.map(|x| (x * 255.0) as u32);
            (
                rgba[0] | rgba[1] << 8 | rgba[2] << 16 | 0xff00_0000,
                id.index(),
            )
        })
        .collect();
    let mut wrong = 0;
    for p in 0..(SIZE * SIZE) as usize {
        let v = f.visbuffer[p];
        if v == 0 {
            continue;
        }
        let row = f.clusters[cluster_of(v)][0];
        match rows.get(&f.color[p]) {
            Some(&expected) if expected == row => {}
            // Shading gradients and edges: only exactly lit faces carry a pure color.
            None => {}
            Some(_) => wrong += 1,
        }
    }
    assert_eq!(wrong, 0, "pixels naming another instance");

    // Moving an instance out of view drops it; the next frame agrees again.
    h.set(
        inside[1],
        marlin_render::scene::InstanceUpdate::Transform(Transform::from_translation(Vec3::new(
            0.0, 50.0, 0.0,
        ))),
    );
    let f = h.visibility_frame(vis);
    assert_eq!(f.counters[0], 3);
    check_against_forward(&f, 0);
}

#[test]
fn back_facing_clusters_are_culled_without_changing_the_picture() {
    let mut h = Harness::new(flat_environment());
    let vis = visibility(&h, 0.0);
    h.renderer.lod_error_pixels = 0.0;
    let s = sphere(32, 64);
    let meshlets = s.view().lods[0].meshlet_count;
    let (ball, _) = h.upload(&s);
    h.camera_at(2.0);
    h.add(ball, Transform::IDENTITY, [1.0; 4]);
    let f = h.visibility_frame(vis);
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
    check_against_forward(&f, 0);
}

#[test]
fn the_gpu_picks_the_lods_the_cpu_does() {
    let mut h = Harness::new(flat_environment());
    let vis = visibility(&h, 1.0);
    let s = sphere(32, 64);
    let triangles: Vec<u64> = s
        .view()
        .lods
        .iter()
        .map(|l| u64::from(l.triangle_count))
        .collect();
    assert!(triangles.len() >= 3);
    let (ball, _) = h.upload(&s);
    h.camera_at(0.0);
    let ids: Vec<_> = [-1.5, -4.0, -12.0, -40.0, -300.0]
        .iter()
        .map(|&z| {
            h.add(
                ball,
                Transform::from_translation(Vec3::new(0.0, 0.0, z)),
                [1.0; 4],
            )
        })
        .collect();
    let f = h.visibility_frame(vis);
    assert_eq!(f.counters[0], ids.len() as u32);
    // Each instance's LOD, from its clusters.
    let mut lods: HashMap<u32, u32> = HashMap::new();
    for c in &f.clusters[..f.counters[2] as usize] {
        let lod = c[1] >> 24;
        assert_eq!(
            *lods.entry(c[0]).or_insert(lod),
            lod,
            "one LOD per instance"
        );
    }
    assert_eq!(lods.len(), ids.len(), "every instance kept a cluster");
    let gpu: u64 = lods.values().map(|&l| triangles[l as usize]).sum();
    // The forward pass's triangles at the CPU's choices.
    let (_, stats) = h.render();
    assert_eq!(gpu, stats.triangles);
    assert!(
        lods.values().any(|&l| l > 0),
        "far instances draw coarser LODs: {lods:?}"
    );
}

#[test]
fn an_empty_view_leaves_the_buffer_clear() {
    let mut h = Harness::new(flat_environment());
    let vis = visibility(&h, 1.0);
    h.camera_at(3.0);
    let f = h.visibility_frame(vis);
    assert_eq!(f.counters, [0, 0, 0]);
    assert!(f.visbuffer.iter().all(|&v| v == 0));
    assert!(f.vis_depth.iter().all(|&d| d == 0.0));
}
