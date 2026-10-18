//! Small scenes under the default lighting, rendered at 64×64 and compared with their references.

use marlin_gpu_test_support::assert_image;
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::scene::MeshId;
use marlin_render::{Camera, Environment, RenderStats};

use crate::support::Harness;
use crate::support::cook::cook;

const ORANGE: [f32; 4] = [0.9, 0.55, 0.2, 1.0];

/// A 2×2 quad in the XZ plane facing +Y.
fn floor(h: &mut Harness) -> MeshId {
    let positions = [
        [-1.0, 0.0, -1.0],
        [-1.0, 0.0, 1.0],
        [1.0, 0.0, 1.0],
        [1.0, 0.0, -1.0],
    ];
    h.upload(&cook(
        &positions,
        &[[0.0, 1.0, 0.0]; 4],
        &[0, 1, 2, 0, 2, 3],
    ))
    .0
}

#[test]
fn lit_cube_from_the_front() {
    let mut h = Harness::new(Environment::default());
    h.camera_at(3.0);
    h.add(h.cube, Transform::IDENTITY, ORANGE);
    let (image, _) = h.render_image();
    assert_image!("lit_cube_front", &image);
}

#[test]
fn rotated_cube_shades_each_face() {
    let mut h = Harness::new(Environment::default());
    h.camera_at(3.0);
    let transform = Transform {
        rotation: Quat::from_euler_yxz(0.7, 0.5, 0.0),
        ..Transform::IDENTITY
    };
    h.add(h.cube, transform, ORANGE);
    let (image, _) = h.render_image();
    assert_image!("rotated_cube", &image);
}

#[test]
fn meshes_overlap_and_intersect_in_depth() {
    let mut h = Harness::new(Environment::default());
    let floor = floor(&mut h);
    // Above and in front, looking a little down.
    h.camera = Some((
        Transform {
            translation: Vec3::new(0.0, 1.5, 4.5),
            rotation: Quat::from_rotation_x(-0.3),
            scale: Vec3::ONE,
        },
        Camera::default(),
    ));
    h.add(
        floor,
        Transform {
            translation: Vec3::new(0.0, -0.5, 0.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::splat(3.0),
        },
        [0.5, 0.5, 0.5, 1.0],
    );
    // A red cube at the back, a green one in front of it overlapping its right half, and a blue
    // one turned 45° pushed halfway into the green one.
    h.add(
        h.cube,
        Transform::from_translation(Vec3::new(-0.6, 0.0, -1.5)),
        [0.9, 0.1, 0.1, 1.0],
    );
    h.add(
        h.cube,
        Transform::from_translation(Vec3::new(-0.2, 0.0, 0.0)),
        [0.1, 0.8, 0.2, 1.0],
    );
    h.add(
        h.cube,
        Transform {
            translation: Vec3::new(0.4, 0.1, 0.2),
            rotation: Quat::from_rotation_y(core::f32::consts::FRAC_PI_4),
            scale: Vec3::splat(0.8),
        },
        [0.2, 0.3, 0.9, 1.0],
    );
    let (image, stats) = h.render_image();
    assert_eq!(
        stats,
        RenderStats {
            instances: 4,
            draws: 2,
            triangles: 38,
        }
    );
    assert_image!("depth_ordering", &image);
}
