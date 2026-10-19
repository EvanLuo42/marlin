//! The scenes: what each holds, and the prefabs.

use marlin_math::{Quat, Transform, Vec3};
use marlin_render::scene::{LightDesc, LightKind};
use marlin_render::{Camera, Environment};

use crate::cook::{Cooked, cook, cube_source};

/// A mesh of a scene: its source triangles (for the CPU reference) and its cooked form.
#[derive(Clone)]
pub struct SceneMesh {
    pub name: &'static str,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

impl SceneMesh {
    /// The unit cube centered on the origin.
    pub fn cube() -> Self {
        let (positions, normals, indices) = cube_source();
        Self {
            name: "Cube",
            positions,
            normals,
            indices,
        }
    }

    /// Cooked by the real mesh cooker.
    pub fn cooked(&self) -> Cooked {
        cook(&self.positions, &self.normals, &self.indices)
    }
}

/// An instance of one of the scene's meshes.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Instance {
    /// Its index in [`Scene::meshes`].
    pub mesh: usize,
    pub transform: Transform,
    /// Its diffuse albedo: the default material's instance color.
    pub albedo: [f32; 3],
}

/// A test scene (see the crate docs).
#[derive(Clone)]
pub struct Scene {
    pub name: &'static str,
    pub meshes: Vec<SceneMesh>,
    pub instances: Vec<Instance>,
    /// In the renderer's units (candela times color for point and spot lights).
    pub lights: Vec<LightDesc>,
    pub environment: Environment,
    pub camera: (Transform, Camera),
}

/// An environment that lights nothing: no sun, no ambient light, a black background. Scenes lit
/// only by their lights.
pub fn dark() -> Environment {
    Environment {
        sun_illuminance: 0.0,
        ambient: [0.0; 3],
        clear_color: [0.0, 0.0, 0.0, 1.0],
        ..Environment::default()
    }
}

/// A point light of `candela` (white times `color`) at `at`, of radius `radius`, reaching
/// `range`.
pub fn point_light(at: Vec3, candela: f32, color: [f32; 3], radius: f32, range: f32) -> LightDesc {
    LightDesc {
        kind: LightKind::Point,
        radiance: color.map(|c| c * candela),
        radius,
        position: [at.x, at.y, at.z],
        direction: [0.0, -1.0, 0.0],
        range,
        cone: [1.0, 0.0],
    }
}

/// A box: the unit cube scaled to `size`, turned `yaw` radians about +Y, centered at `at`.
fn block(at: Vec3, size: Vec3, yaw: f32, albedo: [f32; 3]) -> Instance {
    Instance {
        mesh: 0,
        transform: Transform {
            translation: at,
            rotation: Quat::from_rotation_y(yaw),
            scale: size,
        },
        albedo,
    }
}

/// The camera at `at` looking down -Z, its vertical field of view `fov` degrees.
fn camera(at: Vec3, pitch: f32, fov: f32) -> (Transform, Camera) {
    (
        Transform {
            translation: at,
            rotation: Quat::from_rotation_x(pitch),
            scale: Vec3::ONE,
        },
        Camera {
            fov_y: fov.to_radians(),
            ..Camera::default()
        },
    )
}

/// The Cornell box, after the original's proportions in a 2 m room: floor at y = 0, ceiling at
/// 2, the back wall at z = −1, red on the left (x = −1), green on the right, the front open; a
/// short and a tall block; a sphere light of radius 0.1 just under the ceiling's middle. The
/// camera looks in from 3.4 m in front.
pub fn cornell_box() -> Scene {
    const WHITE: [f32; 3] = [0.73, 0.73, 0.73];
    const RED: [f32; 3] = [0.63, 0.065, 0.05];
    const GREEN: [f32; 3] = [0.14, 0.45, 0.091];
    let t = 0.02;
    let instances = vec![
        block(
            Vec3::new(0.0, -t / 2.0, 0.0),
            Vec3::new(2.0, t, 2.0),
            0.0,
            WHITE,
        ),
        block(
            Vec3::new(0.0, 2.0 + t / 2.0, 0.0),
            Vec3::new(2.0, t, 2.0),
            0.0,
            WHITE,
        ),
        block(
            Vec3::new(0.0, 1.0, -1.0 - t / 2.0),
            Vec3::new(2.0, 2.0, t),
            0.0,
            WHITE,
        ),
        block(
            Vec3::new(-1.0 - t / 2.0, 1.0, 0.0),
            Vec3::new(t, 2.0, 2.0),
            0.0,
            RED,
        ),
        block(
            Vec3::new(1.0 + t / 2.0, 1.0, 0.0),
            Vec3::new(t, 2.0, 2.0),
            0.0,
            GREEN,
        ),
        // The short block, front right; the tall one, back left.
        block(Vec3::new(0.33, 0.3, 0.3), Vec3::splat(0.6), -0.3, WHITE),
        block(
            Vec3::new(-0.35, 0.6, -0.3),
            Vec3::new(0.6, 1.2, 0.6),
            0.3,
            WHITE,
        ),
    ];
    Scene {
        name: "CornellBox",
        meshes: vec![SceneMesh::cube()],
        instances,
        lights: vec![point_light(
            Vec3::new(0.0, 1.85, 0.0),
            10.0,
            [1.0, 0.85, 0.7],
            0.1,
            10.0,
        )],
        environment: dark(),
        camera: camera(Vec3::new(0.0, 1.0, 3.4), 0.0, 40.0),
    }
}

/// A wall filling the view (its front face at z = −1.9, facing the camera at z = 4) and four
/// point lights of 5 to 40 cd in front of it, without size.
pub fn lit_wall() -> Scene {
    let wall = Instance {
        mesh: 0,
        transform: Transform {
            translation: Vec3::new(0.0, 0.0, -2.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::new(20.0, 20.0, 0.2),
        },
        albedo: [0.8, 0.7, 0.6],
    };
    let white = [1.0; 3];
    Scene {
        name: "LitWall",
        meshes: vec![SceneMesh::cube()],
        instances: vec![wall],
        lights: vec![
            point_light(Vec3::new(-1.0, 0.6, -1.0), 5.0, white, 0.0, 50.0),
            point_light(Vec3::new(1.2, 0.5, -1.2), 10.0, white, 0.0, 50.0),
            point_light(Vec3::new(-0.8, -0.7, -0.6), 20.0, white, 0.0, 50.0),
            point_light(Vec3::new(0.9, -0.5, -1.4), 40.0, white, 0.0, 50.0),
        ],
        environment: dark(),
        camera: camera(Vec3::new(0.0, 0.0, 4.0), 0.0, 60.0),
    }
}

/// A 40 m floor with a grid of pillars and `lights` point lights of varied colors and powers
/// over it, seen from above at an angle: many lights, each reaching a few meters.
pub fn light_hall(lights: usize) -> Scene {
    let mut instances = vec![block(
        Vec3::new(0.0, -0.1, 0.0),
        Vec3::new(40.0, 0.2, 40.0),
        0.0,
        [0.6, 0.6, 0.6],
    )];
    for i in -4..=4 {
        for j in -4..=4 {
            instances.push(block(
                Vec3::new(i as f32 * 4.0, 1.5, j as f32 * 4.0),
                Vec3::new(0.5, 3.0, 0.5),
                0.0,
                [0.7, 0.65, 0.6],
            ));
        }
    }
    // A low-discrepancy spread over the floor, hues around the wheel.
    let lights = (0..lights)
        .map(|i| {
            let (u, v) = (fract(i as f32 * 0.754_877_7), fract(i as f32 * 0.569_840_3));
            let at = Vec3::new(
                u * 36.0 - 18.0,
                0.4 + 1.6 * fract(i as f32 * 0.618_034),
                v * 36.0 - 18.0,
            );
            let hue = fract(i as f32 * 0.381_966) * 6.0;
            let color = [
                (hue - 3.0).abs() - 1.0,
                2.0 - (hue - 2.0).abs(),
                2.0 - (hue - 4.0).abs(),
            ]
            .map(|c| c.clamp(0.2, 1.0));
            point_light(
                at,
                5.0 + 45.0 * fract(i as f32 * 0.236_068),
                color,
                0.05,
                6.0,
            )
        })
        .collect();
    Scene {
        name: "LightHall",
        meshes: vec![SceneMesh::cube()],
        instances,
        lights,
        environment: dark(),
        camera: camera(Vec3::new(0.0, 9.0, 16.0), -0.55, 60.0),
    }
}

fn fract(x: f32) -> f32 {
    x - x.floor()
}
