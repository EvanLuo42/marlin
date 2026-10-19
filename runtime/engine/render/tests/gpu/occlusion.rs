//! Two-pass occlusion culling (R2d): what the last frame's HZB hides is culled in pass 1, pass 2
//! draws what pass 1 culled wrongly, and every frame is the picture the reference rasterizer
//! draws of the whole cut, occluded parts and all.

use marlin_math::{Quat, Transform, Vec3};
use marlin_render::Environment;
use marlin_render::scene::{InstanceId, InstanceUpdate};
use marlin_render::visibility::hzb_size;

use crate::support::cook::sphere;
use crate::support::{Harness, SIZE};
use crate::visibility::{check_against_reference, settle};

/// The HZB as the CPU builds it from a depth image (`SIZE` × `SIZE`, row-major): every mip-0
/// texel the farthest (smallest) depth of the pixels it covers, every further mip the smallest
/// of the 2×2 texels under it.
fn cpu_hzb(depth: &[f32]) -> Vec<Vec<f32>> {
    let ([w0, h0], mips) = hzb_size(SIZE, SIZE);
    let mut out: Vec<Vec<f32>> = Vec::new();
    let mut first = vec![0.0; (w0 * h0) as usize];
    for y in 0..h0 {
        for x in 0..w0 {
            let (x0, y0) = (x * SIZE / w0, y * SIZE / h0);
            let x1 = ((x + 1) * SIZE).div_ceil(w0).min(SIZE);
            let y1 = ((y + 1) * SIZE).div_ceil(h0).min(SIZE);
            let mut farthest = 1.0f32;
            for py in y0..y1 {
                for px in x0..x1 {
                    farthest = farthest.min(depth[(py * SIZE + px) as usize]);
                }
            }
            first[(y * w0 + x) as usize] = farthest;
        }
    }
    out.push(first);
    for mip in 1..mips {
        let (sw, sh) = ((w0 >> (mip - 1)).max(1), (h0 >> (mip - 1)).max(1));
        let (w, h) = ((w0 >> mip).max(1), (h0 >> mip).max(1));
        let below = &out[mip as usize - 1];
        let at = |x: u32, y: u32| below[(y.min(sh - 1) * sw + x.min(sw - 1)) as usize];
        let mut level = vec![0.0; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                level[(y * w + x) as usize] = at(2 * x, 2 * y)
                    .min(at(2 * x + 1, 2 * y))
                    .min(at(2 * x, 2 * y + 1))
                    .min(at(2 * x + 1, 2 * y + 1));
            }
        }
        out.push(level);
    }
    out
}

/// A wall filling the view from (0, 0, 4), and three balls behind it.
fn wall_scene(h: &mut Harness) -> (InstanceId, Vec<InstanceId>) {
    h.vis.lod_error_pixels = 1.0;
    let (ball, _) = h.upload(&sphere(32, 64));
    h.camera_at(4.0);
    let wall = h.add(
        h.cube,
        Transform {
            translation: Vec3::new(0.0, 0.0, 1.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::new(8.0, 8.0, 0.2),
        },
        [0.5, 0.5, 0.5, 1.0],
    );
    let balls = [-1.0, 0.0, 1.0]
        .map(|x| {
            h.add(
                ball,
                Transform::from_translation(Vec3::new(x, 0.0, -2.0)),
                [1.0, 0.0, 0.0, 1.0],
            )
        })
        .to_vec();
    (wall, balls)
}

#[test]
fn occluders_hide_what_is_behind_them() {
    let mut h = Harness::new(Environment::default());
    let (wall, balls) = wall_scene(&mut h);
    settle(&mut h);
    let f = h.visibility_frame();
    check_against_reference(&h, &f);
    assert!(f.two_pass);
    // Pass 1 culls the balls behind the last frame's wall; pass 2 finds them hidden still.
    assert_eq!(f.occluded_instances, balls.len() as u32);
    let drawn = f.counters[2] as usize;
    assert!(drawn > 0);
    assert!(
        f.clusters[..drawn].iter().all(|c| c[0] == wall.index()),
        "only the wall is drawn"
    );

    // Without occlusion culling the balls' clusters are drawn too, behind the wall.
    h.vis.occlusion = false;
    let without = h.visibility_frame();
    check_against_reference(&h, &without);
    assert!(!without.two_pass);
    assert!(without.counters[2] as usize > drawn);
    assert_eq!(without.color, f.color, "the same picture");
}

#[test]
fn what_comes_out_from_behind_is_drawn_the_same_frame() {
    let mut h = Harness::new(Environment::default());
    let (wall, balls) = wall_scene(&mut h);
    settle(&mut h);
    // The wall moves away: pass 1 still culls the balls by the last frame's HZB, pass 2 sees
    // them in front of nothing and draws them.
    h.set(
        wall,
        InstanceUpdate::Transform(Transform::from_translation(Vec3::new(40.0, 0.0, 0.0))),
    );
    let f = h.visibility_frame();
    check_against_reference(&h, &f);
    assert!(f.two_pass);
    assert_eq!(f.occluded_instances, balls.len() as u32);
    let drawn = f.counters[2] as usize;
    let rows: Vec<u32> = f.clusters[f.pass_first_cluster as usize..drawn]
        .iter()
        .map(|c| c[0])
        .collect();
    for ball in &balls {
        assert!(
            rows.contains(&ball.index()),
            "ball {} drawn in pass 2",
            ball.index()
        );
    }

    // The camera turns and moves: pass 1 tests with the last frame's view, pass 2 with this one.
    for (i, yaw) in [0.4f32, -0.3, 0.9].into_iter().enumerate() {
        h.camera = Some((
            Transform {
                translation: Vec3::new(0.3 * i as f32, 0.0, 3.0 - i as f32),
                rotation: Quat::from_rotation_y(yaw),
                scale: Vec3::ONE,
            },
            marlin_render::Camera::default(),
        ));
        let f = h.visibility_frame();
        check_against_reference(&h, &f);
    }
}

#[test]
fn the_hzb_holds_the_farthest_depth_under_each_texel() {
    let mut h = Harness::new(Environment::default());
    let (_, _) = wall_scene(&mut h);
    // Something with depth to tell apart: a ball in front of the wall.
    let (ball, _) = h.upload(&sphere(16, 32));
    h.add(
        ball,
        Transform::from_translation(Vec3::new(0.4, 0.2, 2.0)),
        [0.0, 1.0, 0.0, 1.0],
    );
    let f = settle(&mut h);
    let expected = cpu_hzb(&f.vis_depth);
    assert_eq!(f.hzb.len(), expected.len());
    for (mip, (gpu, cpu)) in f.hzb.iter().zip(&expected).enumerate() {
        assert_eq!(gpu, cpu, "mip {mip}");
    }
    assert_eq!(f.hzb.last().unwrap().len(), 1, "down to one texel");
}

#[test]
fn clusters_behind_an_occluder_are_culled_one_by_one() {
    let mut h = Harness::new(Environment::default());
    h.vis.lod_error_pixels = 0.0;
    // The finest clusters: small enough to be behind the wall alone.
    let (ball, _) = h.upload(&sphere(32, 64));
    h.camera_at(4.0);
    // A wall over the left half of the view, a ball across both halves behind it: the instance
    // is visible, its clusters on the left are not.
    let wall = h.add(
        h.cube,
        Transform {
            translation: Vec3::new(-2.0, 0.0, 1.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::new(4.0, 8.0, 0.2),
        },
        [0.5, 0.5, 0.5, 1.0],
    );
    let big = h.add(
        ball,
        Transform {
            translation: Vec3::new(0.0, 0.0, -2.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::splat(4.0),
        },
        [1.0, 0.0, 0.0, 1.0],
    );
    settle(&mut h);
    let f = h.visibility_frame();
    check_against_reference(&h, &f);
    assert_eq!(f.occluded_instances, 0);
    assert!(f.occluded_clusters > 0, "clusters behind the wall");
    let drawn = f.counters[2] as usize;
    assert!(f.clusters[..drawn].iter().any(|c| c[0] == big.index()));
    assert!(f.clusters[..drawn].iter().any(|c| c[0] == wall.index()));

    // The wall moves away: pass 2 draws the clusters pass 1 culled.
    h.set(
        wall,
        InstanceUpdate::Transform(Transform::from_translation(Vec3::new(40.0, 0.0, 0.0))),
    );
    let f = h.visibility_frame();
    check_against_reference(&h, &f);
    assert!(f.occluded_clusters > 0);
    let drawn = f.counters[2] as usize;
    assert!(
        f.clusters[f.pass_first_cluster as usize..drawn]
            .iter()
            .any(|c| c[0] == big.index()),
        "pass 2 drew the ball's clusters"
    );
}
