//! The forward renderer's draws, instancing, depth and mesh handles, checked pixel by pixel.

use marlin_math::{Quat, Transform, Vec3};
use marlin_render::scene::{InstanceUpdate, MeshUpdate, flags};
use marlin_render::{Environment, MeshData, RenderStats, cube};

use crate::support::Harness;

const BLACK: u32 = 0xff00_0000;
const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

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

fn pixel(pixels: &[u32], x: u32, y: u32) -> u32 {
    pixels[(y * 64 + x) as usize]
}

#[test]
fn draws_a_cube_in_front_of_the_camera() {
    let mut h = Harness::new(flat_environment());
    h.camera_at(3.0);
    h.add(h.cube, Transform::IDENTITY, RED);
    let (pixels, stats) = h.render();
    assert_eq!(
        stats,
        RenderStats {
            instances: 1,
            draws: 1
        }
    );
    assert_eq!(pixel(&pixels, 32, 32), 0xff00_00ff, "the red front face");
    assert_eq!(pixel(&pixels, 1, 1), BLACK, "background");
}

#[test]
fn instances_of_one_mesh_share_a_draw_and_depth_sorts_them() {
    let mut h = Harness::new(flat_environment());
    h.camera_at(5.0);
    let colors = [[0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]];
    for (x, color) in [-1.0, 1.0].into_iter().zip(colors) {
        h.add(
            h.cube,
            Transform::from_translation(Vec3::new(x, 0.0, 0.0)),
            color,
        );
    }
    // A white cube behind the left one, partly hidden by it.
    h.add(
        h.cube,
        Transform {
            translation: Vec3::new(-1.0, 0.0, -2.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::splat(3.0),
        },
        [1.0, 1.0, 1.0, 1.0],
    );
    let (pixels, stats) = h.render();
    assert_eq!(
        stats,
        RenderStats {
            instances: 3,
            draws: 1
        }
    );
    // Cube centers project to x ≈ 32 ± 12.3.
    assert_eq!(
        pixel(&pixels, 20, 32),
        0xff00_ff00,
        "green, in front of white"
    );
    assert_eq!(pixel(&pixels, 44, 32), 0xffff_0000, "blue");
    assert_eq!(
        pixel(&pixels, 8, 32),
        0xffff_ffff,
        "the white cube around green"
    );
}

#[test]
fn without_a_camera_only_clears() {
    let mut h = Harness::new(flat_environment());
    h.add(h.cube, Transform::IDENTITY, RED);
    let (pixels, stats) = h.render();
    assert_eq!(stats, RenderStats::default());
    assert!(pixels.iter().all(|&p| p == BLACK));
}

#[test]
fn updates_change_only_what_they_name() {
    let mut h = Harness::new(flat_environment());
    h.camera_at(3.0);
    let cube = h.add(h.cube, Transform::IDENTITY, RED);
    h.render();
    for i in 0..3 {
        let mut c = [0.0, 0.0, 0.0, 1.0];
        c[i] = 1.0;
        h.set(cube, InstanceUpdate::Color(c));
        let (pixels, _) = h.render();
        assert_eq!(pixel(&pixels, 32, 32), 0xff00_0000 | (0xff << (8 * i)));
    }
    // A frame without updates draws the same.
    let (pixels, _) = h.render();
    assert_eq!(pixel(&pixels, 32, 32), 0xffff_0000);

    // Moved out of view, then hidden, then back.
    h.set(
        cube,
        InstanceUpdate::Transform(Transform::from_translation(Vec3::new(10.0, 0.0, 0.0))),
    );
    let (pixels, _) = h.render();
    assert!(pixels.iter().all(|&p| p == BLACK), "moved away");
    h.set(cube, InstanceUpdate::Transform(Transform::IDENTITY));
    h.set(cube, InstanceUpdate::Flags(flags::HIDDEN));
    let (pixels, stats) = h.render();
    assert_eq!(stats.instances, 0, "hidden");
    assert!(pixels.iter().all(|&p| p == BLACK));
    h.set(cube, InstanceUpdate::Flags(0));
    let (pixels, _) = h.render();
    assert_eq!(pixel(&pixels, 32, 32), 0xffff_0000, "back");

    // Removed: its handle is dead, and the row comes back with a new generation.
    h.set(cube, InstanceUpdate::Remove);
    let (pixels, stats) = h.render();
    assert_eq!(stats, RenderStats::default());
    assert!(pixels.iter().all(|&p| p == BLACK));
    h.set(cube, InstanceUpdate::Color(RED));
    let again = h.add(h.cube, Transform::IDENTITY, RED);
    assert_eq!(again.index(), cube.index());
    assert_ne!(again, cube);
    let (pixels, stats) = h.render();
    assert_eq!(stats.instances, 1);
    assert_eq!(pixel(&pixels, 32, 32), 0xff00_00ff);
}

#[test]
fn a_mesh_draws_once_resident_and_a_released_one_draws_nothing() {
    let mut h = Harness::new(flat_environment());
    h.camera_at(3.0);
    let (vertices, indices) = cube();
    let data = MeshData {
        vertices: &vertices,
        indices: &indices,
    };
    // Referenced, not resident yet.
    let mesh = h.scene.ids().meshes.reserve();
    h.updates.meshes.push(mesh, MeshUpdate::Add);
    h.add(mesh, Transform::IDENTITY, RED);
    let (pixels, stats) = h.render();
    assert_eq!(stats.draws, 0);
    assert!(pixels.iter().all(|&p| p == BLACK));

    // Resident on a mesh the renderer then releases: nothing is drawn with the dead handle.
    let old = h.renderer.upload_mesh(h.device, &data).unwrap();
    let resident = h.resident(old, &data);
    h.updates.meshes.push(mesh, MeshUpdate::Resident(resident));
    let (pixels, stats) = h.render();
    assert_eq!(stats.draws, 1);
    assert_eq!(pixel(&pixels, 32, 32), 0xff00_00ff);
    h.wait_idle();
    // SAFETY: the GPU is idle.
    unsafe { h.renderer.release_mesh(old) };
    let new = h.renderer.upload_mesh(h.device, &data).unwrap();
    assert_ne!(new, old);
    let (pixels, stats) = h.render();
    assert_eq!(stats.draws, 0, "the old handle draws nothing");
    assert!(pixels.iter().all(|&p| p == BLACK));

    // A reload: the new copy replaces it.
    let resident = h.resident(new, &data);
    h.updates.meshes.push(mesh, MeshUpdate::Resident(resident));
    let (pixels, stats) = h.render();
    assert_eq!(stats.draws, 1);
    assert_eq!(pixel(&pixels, 32, 32), 0xff00_00ff);

    // Removed from the scene: its instances stay but draw nothing.
    h.updates.meshes.push(mesh, MeshUpdate::Remove);
    let (_, stats) = h.render();
    assert_eq!(stats.draws, 0);
    assert_eq!(h.scene.instance_count(), 1);
}
