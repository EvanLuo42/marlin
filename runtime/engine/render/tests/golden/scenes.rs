//! Small scenes under the default lighting, rendered at 64×64 and compared with their references.

use marlin_ecs::World;
use marlin_gpu_test_support::assert_image;
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::{Camera, Environment, MeshData, MeshHandle, MeshRenderer, RenderStats, Vertex};

use crate::support::{Harness, camera};

const ORANGE: [f32; 4] = [0.9, 0.55, 0.2, 1.0];

fn cube(world: &mut World, mesh: MeshHandle, transform: Transform, color: [f32; 4]) {
    world.spawn((transform, MeshRenderer { mesh, color }));
}

/// A 2×2 quad in the XZ plane facing +Y.
fn floor(h: &mut Harness) -> MeshHandle {
    let vertex = |x, z| Vertex {
        position: [x, 0.0, z],
        normal: [0.0, 1.0, 0.0],
    };
    let vertices = [
        vertex(-1.0, -1.0),
        vertex(-1.0, 1.0),
        vertex(1.0, 1.0),
        vertex(1.0, -1.0),
    ];
    h.renderer
        .upload_mesh(
            h.device,
            &MeshData {
                vertices: &vertices,
                indices: &[0, 1, 2, 0, 2, 3],
            },
        )
        .unwrap()
}

#[test]
fn lit_cube_from_the_front() {
    let mut h = Harness::new(Environment::default());
    let mut world = World::new();
    camera(&mut world, 3.0);
    cube(&mut world, h.cube, Transform::IDENTITY, ORANGE);
    let (image, _) = h.render_image(&world);
    assert_image!("lit_cube_front", &image);
}

#[test]
fn rotated_cube_shades_each_face() {
    let mut h = Harness::new(Environment::default());
    let mut world = World::new();
    camera(&mut world, 3.0);
    let transform = Transform {
        rotation: Quat::from_euler_yxz(0.7, 0.5, 0.0),
        ..Transform::IDENTITY
    };
    cube(&mut world, h.cube, transform, ORANGE);
    let (image, _) = h.render_image(&world);
    assert_image!("rotated_cube", &image);
}

#[test]
fn meshes_overlap_and_intersect_in_depth() {
    let mut h = Harness::new(Environment::default());
    let floor = floor(&mut h);
    let mut world = World::new();
    // Above and in front, looking a little down.
    world.spawn((
        Transform {
            translation: Vec3::new(0.0, 1.5, 4.5),
            rotation: Quat::from_rotation_x(-0.3),
            scale: Vec3::ONE,
        },
        Camera::default(),
    ));
    world.spawn((
        Transform {
            translation: Vec3::new(0.0, -0.5, 0.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::splat(3.0),
        },
        MeshRenderer {
            mesh: floor,
            color: [0.5, 0.5, 0.5, 1.0],
        },
    ));
    // A red cube at the back, a green one in front of it overlapping its right half, and a blue
    // one turned 45° pushed halfway into the green one.
    cube(
        &mut world,
        h.cube,
        Transform::from_translation(Vec3::new(-0.6, 0.0, -1.5)),
        [0.9, 0.1, 0.1, 1.0],
    );
    cube(
        &mut world,
        h.cube,
        Transform::from_translation(Vec3::new(-0.2, 0.0, 0.0)),
        [0.1, 0.8, 0.2, 1.0],
    );
    cube(
        &mut world,
        h.cube,
        Transform {
            translation: Vec3::new(0.4, 0.1, 0.2),
            rotation: Quat::from_rotation_y(core::f32::consts::FRAC_PI_4),
            scale: Vec3::splat(0.8),
        },
        [0.2, 0.3, 0.9, 1.0],
    );
    let (image, stats) = h.render_image(&world);
    assert_eq!(
        stats,
        RenderStats {
            instances: 4,
            draws: 2
        }
    );
    assert_image!("depth_ordering", &image);
}
