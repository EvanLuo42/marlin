//! The software raster (R2e): clusters whose triangles are small on screen are drawn by a
//! compute shader into the same visibility buffer, covering exactly the pixels the hardware
//! would, at depths a hair apart; clusters reaching the near plane stay on the hardware.

use marlin_math::{Quat, Transform, Vec3};
use marlin_render::Environment;

use crate::support::cook::sphere;
use crate::support::{Harness, SIZE, VisFrame};
use crate::visibility::{check_against_reference, settle};

/// A few balls and boxes at different sizes, every page streamed in.
fn scene(h: &mut Harness) {
    h.vis.lod_error_pixels = 0.0;
    let (ball, _) = h.upload(&sphere(32, 64));
    h.camera_at(4.0);
    for (i, &(x, y, z, s)) in [
        (-1.2, 0.6, 0.0, 1.0),
        (0.8, -0.5, -1.0, 2.0),
        (0.3, 0.9, 1.5, 0.5),
        (-0.4, -0.8, 0.5, 1.2),
    ]
    .iter()
    .enumerate()
    {
        h.add(
            if i % 2 == 0 { ball } else { h.cube },
            Transform {
                translation: Vec3::new(x, y, z),
                rotation: Quat::from_euler_yxz(0.3 * i as f32, 0.2, 0.0),
                scale: Vec3::splat(s),
            },
            [0.2 * i as f32, 1.0, 0.0, 1.0],
        );
    }
    settle(h);
}

/// The frame with clusters routed to the software raster below `edge` pixels.
fn frame_at(h: &mut Harness, edge: f32) -> VisFrame {
    h.vis.sw_edge_pixels = edge;
    let f = h.visibility_frame();
    check_against_reference(h, &f);
    f
}

#[test]
fn the_software_raster_covers_what_the_hardware_does() {
    let mut h = Harness::new(Environment::default());
    scene(&mut h);
    let hardware = frame_at(&mut h, 0.0);
    assert_eq!(hardware.sw_drawn, 0, "everything on the hardware");
    let software = frame_at(&mut h, 1.0e9);
    assert_eq!(software.hw_drawn, 0, "everything in software");
    assert_eq!(software.sw_drawn, hardware.hw_drawn);
    for p in 0..(SIZE * SIZE) as usize {
        let (a, b) = (hardware.visbuffer[p], software.visbuffer[p]);
        assert_eq!(a == 0, b == 0, "pixel {p}: covered by one only");
        if a == 0 {
            continue;
        }
        // The same instance's surface, at the same depth but for interpolation.
        let row = |f: &VisFrame, v: u64| f.clusters[((v >> 7) & ((1 << 25) - 1)) as usize][0];
        assert_eq!(row(&hardware, a), row(&software, b), "pixel {p}");
        let (da, db) = (
            f32::from_bits((a >> 32) as u32),
            f32::from_bits((b >> 32) as u32),
        );
        assert!((da - db).abs() <= da * 5e-4, "pixel {p}: {da} and {db}");
    }
    assert_eq!(
        hardware.color, software.color,
        "the reference saw the same scene"
    );
}

#[test]
fn small_triangles_go_to_software_and_large_ones_to_hardware() {
    let mut h = Harness::new(Environment::default());
    scene(&mut h);
    // The default: in a 64-pixel view most clusters' triangles are a few pixels.
    let f = frame_at(&mut h, 32.0);
    assert!(
        f.sw_drawn > 0 && f.sw_drawn > f.hw_drawn,
        "{} in software, {} on hardware",
        f.sw_drawn,
        f.hw_drawn
    );
    // A close-up of a big box: its few triangles span the view.
    let mut h = Harness::new(Environment::default());
    h.camera_at(2.0);
    h.add(h.cube, Transform::IDENTITY, [1.0; 4]);
    settle(&mut h);
    let f = frame_at(&mut h, 32.0);
    assert!(f.hw_drawn > 0 && f.sw_drawn == 0);
}

#[test]
fn clusters_reaching_the_near_plane_stay_on_the_hardware() {
    let mut h = Harness::new(Environment::default());
    h.vis.lod_error_pixels = 0.0;
    let (ball, _) = h.upload(&sphere(32, 64));
    // The camera inside a big ball, looking at its far wall: the clusters around the camera
    // cross the near plane.
    h.camera_at(0.0);
    h.add(ball, Transform::from_scale(Vec3::splat(6.0)), [1.0; 4]);
    h.add(
        ball,
        Transform::from_translation(Vec3::new(0.0, 0.0, -0.11)),
        [0.0, 1.0, 0.0, 1.0],
    );
    settle(&mut h);
    let f = frame_at(&mut h, 1.0e9);
    assert!(f.hw_drawn > 0, "the near clusters on the hardware");
    assert!(f.sw_drawn > 0, "the rest in software");
}
