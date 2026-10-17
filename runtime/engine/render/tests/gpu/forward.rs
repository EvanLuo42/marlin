//! The forward renderer's draws, instancing, depth and mesh handles, checked pixel by pixel.

use marlin_ecs::World;
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::{Environment, MeshData, MeshRenderer, RenderStats, cube};

use crate::support::{Harness, camera};

const BLACK: u32 = 0xff00_0000;

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
    let mut world = World::new();
    camera(&mut world, 3.0);
    world.spawn((
        Transform::IDENTITY,
        MeshRenderer {
            mesh: h.cube,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    let (pixels, stats) = h.render(&world);
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
    let mut world = World::new();
    camera(&mut world, 5.0);
    let colors = [[0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]];
    for (x, color) in [-1.0, 1.0].into_iter().zip(colors) {
        world.spawn((
            Transform::from_translation(Vec3::new(x, 0.0, 0.0)),
            MeshRenderer {
                mesh: h.cube,
                color,
            },
        ));
    }
    // A white cube behind the left one, partly hidden by it.
    world.spawn((
        Transform {
            translation: Vec3::new(-1.0, 0.0, -2.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::splat(3.0),
        },
        MeshRenderer {
            mesh: h.cube,
            color: [1.0, 1.0, 1.0, 1.0],
        },
    ));
    let (pixels, stats) = h.render(&world);
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
    let mut world = World::new();
    world.spawn((
        Transform::IDENTITY,
        MeshRenderer {
            mesh: h.cube,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    let (pixels, stats) = h.render(&world);
    assert_eq!(stats, RenderStats::default());
    assert!(pixels.iter().all(|&p| p == BLACK));
}

#[test]
fn renders_repeatedly_into_the_same_slot() {
    let mut h = Harness::new(flat_environment());
    let mut world = World::new();
    camera(&mut world, 3.0);
    let cube = world.spawn((
        Transform::IDENTITY,
        MeshRenderer {
            mesh: h.cube,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    for i in 0..3 {
        let color = [0.0, 0.0, 0.0, 1.0].map(|_| 0.0);
        let mut c = color;
        c[i] = 1.0;
        c[3] = 1.0;
        world.get_mut::<MeshRenderer>(cube).unwrap().color = c;
        let (pixels, _) = h.render(&world);
        assert_eq!(pixel(&pixels, 32, 32), 0xff00_0000 | (0xff << (8 * i)));
    }
}

#[test]
fn a_released_handle_stays_dead_after_its_slot_is_reused() {
    let mut h = Harness::new(flat_environment());
    let (vertices, indices) = cube();
    let mesh = MeshData {
        vertices: &vertices,
        indices: &indices,
    };
    let old = h.cube;
    // SAFETY: nothing has drawn the mesh yet.
    unsafe { h.renderer.release_mesh(old) };
    assert_eq!(h.renderer.mesh_count(), 0);
    let new = h.renderer.upload_mesh(h.device, &mesh).unwrap();
    assert_ne!(new, old);
    assert_eq!(h.renderer.mesh_count(), 1);

    let mut world = World::new();
    camera(&mut world, 3.0);
    let entity = world.spawn((
        Transform::IDENTITY,
        MeshRenderer {
            mesh: old,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    let (pixels, stats) = h.render(&world);
    assert_eq!(stats.draws, 0, "the old handle draws nothing");
    assert!(pixels.iter().all(|&p| p == BLACK));

    world.get_mut::<MeshRenderer>(entity).unwrap().mesh = new;
    let (pixels, stats) = h.render(&world);
    assert_eq!(stats.draws, 1);
    assert_eq!(pixel(&pixels, 32, 32), 0xff00_00ff);
}
