//! Levels: what a world starts with, authored as `.masset` text.
//!
//! ```text
//! Level
//! {
//!     !Uuid = "…"
//!     !Name = "Main"
//!     Environment = { ClearColor = (0.02, 0.02, 0.03, 1.0) }
//!     Entities = [
//!         { Name = "Camera" Transform = { Translation = (0.0, 2.0, 8.0) } Components = [Camera { FovY = 60.0 }, FlyCamera { Speed = 10.0 }] },
//!         { Name = "Rock" Components = [StaticMesh { Mesh = <meshes/rock:Rock #…> Color = (0.5, 0.5, 0.5, 1.0) }] },
//!     ]
//! }
//! ```
//!
//! A level links the meshes it places, so loading it loads them (the asset server loads a unit's
//! imports with it). Components are the engine's [`Component`] kinds for now; script components
//! join them with the VM.

use alloc::string::String;
use alloc::vec::Vec;

use marlin_asset::Link;
use marlin_math::{Quat, Transform, Vec3};
use marlin_reflect::Reflect;
use marlin_render::asset::Mesh;

#[derive(Clone, PartialEq, Debug, Reflect)]
#[reflect(default)]
#[derive(Default)]
pub struct Level {
    pub environment: LevelEnvironment,
    pub entities: Vec<LevelEntity>,
}

/// Lighting and background (see `marlin_render::Environment`).
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(default)]
pub struct LevelEnvironment {
    /// The direction light travels.
    pub light_direction: [f32; 3],
    /// Linear radiance.
    pub light_color: [f32; 3],
    pub ambient: [f32; 3],
    /// Linear RGBA.
    pub clear_color: [f32; 4],
    /// The light's angular diameter, in degrees: how soft its shadows are.
    pub sun_angular_diameter: f32,
}

impl Default for LevelEnvironment {
    fn default() -> Self {
        let e = marlin_render::Environment::default();
        Self {
            light_direction: [
                e.light_direction.x,
                e.light_direction.y,
                e.light_direction.z,
            ],
            light_color: e.light_color,
            ambient: e.ambient,
            clear_color: e.clear_color,
            sun_angular_diameter: marlin_render::SUN_ANGULAR_DIAMETER_DEGREES,
        }
    }
}

impl LevelEnvironment {
    pub fn to_render(&self) -> marlin_render::Environment {
        let [x, y, z] = self.light_direction;
        marlin_render::Environment {
            light_direction: Vec3::new(x, y, z),
            light_color: self.light_color,
            ambient: self.ambient,
            clear_color: self.clear_color,
            sun_angular_diameter: self.sun_angular_diameter.to_radians(),
        }
    }
}

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct LevelEntity {
    pub name: String,
    pub transform: TransformDesc,
    pub components: Vec<Component>,
}

/// A transform as authored: Euler angles in degrees (yaw about Y, pitch about X, roll about Z,
/// applied in that order, as cameras and characters turn).
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(default)]
pub struct TransformDesc {
    pub translation: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
}

impl Default for TransformDesc {
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            rotation: [0.0; 3],
            scale: [1.0; 3],
        }
    }
}

impl TransformDesc {
    pub fn to_transform(&self) -> Transform {
        let [t, r, s] = [self.translation, self.rotation, self.scale];
        let rad = r.map(f32::to_radians);
        Transform {
            translation: Vec3::new(t[0], t[1], t[2]),
            rotation: Quat::from_euler_yxz(rad[0], rad[1], rad[2]),
            scale: Vec3::new(s[0], s[1], s[2]),
        }
    }
}

/// What an entity is made of.
#[derive(Clone, PartialEq, Debug, Reflect)]
pub enum Component {
    /// Draws a mesh at the entity's transform.
    StaticMesh { mesh: Link<Mesh>, color: [f32; 4] },
    /// A view into the world. The first camera is the one rendered.
    Camera { fov_y: f32, near: f32 },
    /// Turns the entity about `axis` at `speed` degrees per second.
    Spin { axis: [f32; 3], speed: f32 },
    /// Mouse and keyboard (or gamepad) control: right button to look, WASD to move.
    FlyCamera { speed: f32 },
}
