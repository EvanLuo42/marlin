//! Small scenes under the default lighting, rendered at 64×64 through the whole pipeline
//! (visibility, materials, lighting) and compared with their references, which the forward
//! renderer drew before the pipeline replaced it (R1l): the pictures agree within the
//! references' tolerance.

use marlin_gpu_test_support::assert_image;
use marlin_material::DefaultMaterial;
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::Camera;
use marlin_render::scene::{MaterialId, MeshId};

use crate::support::Harness;
use crate::support::cook::{cook, cube};

const ORANGE: [f32; 4] = [0.9, 0.55, 0.2, 1.0];
const SIZE: u32 = 64;

/// The cube, the default material (white: the instance colors are the albedos) and a camera at
/// (0, 0, `z`) looking down -Z.
fn setup(h: &mut Harness, z: f32) -> (MeshId, MaterialId) {
    let c = h.upload(&cube());
    let m = h.default_material(DefaultMaterial::default());
    h.camera = (
        Transform::from_translation(Vec3::new(0.0, 0.0, z)),
        Camera::default(),
    );
    (c, m)
}

#[test]
fn lit_cube_from_the_front() {
    let mut h = Harness::new();
    let (c, m) = setup(&mut h, 3.0);
    h.add(c, m, Transform::IDENTITY, ORANGE);
    assert_image!("lit_cube_front", &h.lit_image(SIZE, SIZE));
}

#[test]
fn rotated_cube_shades_each_face() {
    let mut h = Harness::new();
    let (c, m) = setup(&mut h, 3.0);
    let transform = Transform {
        rotation: Quat::from_euler_yxz(0.7, 0.5, 0.0),
        ..Transform::IDENTITY
    };
    h.add(c, m, transform, ORANGE);
    assert_image!("rotated_cube", &h.lit_image(SIZE, SIZE));
}

#[test]
fn meshes_overlap_and_intersect_in_depth() {
    let mut h = Harness::new();
    let (c, m) = setup(&mut h, 0.0);
    // A 2×2 quad in the XZ plane facing +Y.
    let positions = [
        [-1.0, 0.0, -1.0],
        [-1.0, 0.0, 1.0],
        [1.0, 0.0, 1.0],
        [1.0, 0.0, -1.0],
    ];
    let floor = h.upload(&cook(
        &positions,
        &[[0.0, 1.0, 0.0]; 4],
        &[0, 1, 2, 0, 2, 3],
    ));
    // Above and in front, looking a little down.
    h.camera = (
        Transform {
            translation: Vec3::new(0.0, 1.5, 4.5),
            rotation: Quat::from_rotation_x(-0.3),
            scale: Vec3::ONE,
        },
        Camera::default(),
    );
    h.add(
        floor,
        m,
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
        c,
        m,
        Transform::from_translation(Vec3::new(-0.6, 0.0, -1.5)),
        [0.9, 0.1, 0.1, 1.0],
    );
    h.add(
        c,
        m,
        Transform::from_translation(Vec3::new(-0.2, 0.0, 0.0)),
        [0.1, 0.8, 0.2, 1.0],
    );
    h.add(
        c,
        m,
        Transform {
            translation: Vec3::new(0.4, 0.1, 0.2),
            rotation: Quat::from_rotation_y(core::f32::consts::FRAC_PI_4),
            scale: Vec3::splat(0.8),
        },
        [0.2, 0.3, 0.9, 1.0],
    );
    assert_image!("depth_ordering", &h.lit_image(SIZE, SIZE));
}
