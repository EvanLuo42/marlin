//! The engine's components that levels place, in authored units (0019 §5.1, §7).
//!
//! Each is one reflected type, saved in levels as written (`MeshRenderer { Mesh = <…> }`),
//! with its defaults in `Default`. Conversions to what the renderer wants (lumens to candela,
//! degrees to cosines) happen in extract ([`crate::engine`]). These move to the render and
//! gameplay modules with the module system (0019 §14, W3); their text names stay.
//!
//! ```text
//! Level
//! {
//!     !Uuid = "…"
//!     !Name = "Main"
//!     Entities = [
//!         {
//!             Guid = "…"
//!             Name = "Camera"
//!             Components = [Transform { Translation = (0.0, 2.0, 8.0) }, Camera, SpectatorMovement]
//!         },
//!         {
//!             Guid = "…"
//!             Name = "Rock"
//!             Components = [Transform, MeshRenderer { Mesh = <meshes/rock:Rock #…> }]
//!         },
//!     ]
//! }
//! ```

use alloc::sync::Arc;

use marlin_asset::Link;
use marlin_math::{Quat, Vec3};
use marlin_reflect::Reflect;
use marlin_render::asset::Mesh;
use marlin_world::{ComponentFlags, ComponentRegistry};

/// Draws a mesh at the entity's transform.
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(name = "marlin_framework::MeshRenderer", default)]
pub struct MeshRenderer {
    pub mesh: Link<Mesh>,
    /// Linear RGBA the default material is tinted with.
    #[reflect(widget = Color)]
    pub color: [f32; 4],
}

impl Default for MeshRenderer {
    fn default() -> Self {
        Self {
            mesh: Link::NONE,
            color: [1.0; 4],
        }
    }
}

/// A view into the world. The first camera found is the one rendered (until views, 0019 §8).
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(name = "marlin_framework::Camera", default)]
pub struct Camera {
    /// Full vertical field of view.
    #[reflect(units = "°", range = 1.0..=179.0)]
    pub fov_y: f32,
    /// Near plane distance; the far plane is at infinity.
    #[reflect(units = "m", range = 0.001..)]
    pub near: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            fov_y: 60.0,
            near: 0.1,
        }
    }
}

impl Camera {
    pub fn to_render(&self) -> marlin_render::Camera {
        marlin_render::Camera {
            fov_y: self.fov_y.clamp(1.0, 179.0).to_radians(),
            near: self.near.max(1e-4),
            ..marlin_render::Camera::default()
        }
    }
}

/// Light in every direction from the entity's position (0010 §5.1).
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(name = "marlin_framework::PointLight", default)]
pub struct PointLight {
    #[reflect(widget = Color)]
    pub color: [f32; 3],
    /// Luminous flux (a 60 W bulb: 800).
    #[reflect(units = "lm", range = 0.0..)]
    pub intensity: f32,
    /// The emitting sphere's radius.
    #[reflect(units = "m", range = 0.0..)]
    pub radius: f32,
    /// Where its light fades out.
    #[reflect(units = "m", range = 0.01..)]
    pub range: f32,
}

impl Default for PointLight {
    fn default() -> Self {
        Self {
            color: [1.0; 3],
            intensity: 800.0,
            radius: 0.05,
            range: 10.0,
        }
    }
}

/// A point light shining down the entity's -Z, full inside `inner_angle` and fading to none at
/// `outer_angle` (half angles). Its lumens are a point light's: narrowing the cone does not
/// brighten it.
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(name = "marlin_framework::SpotLight", default)]
pub struct SpotLight {
    #[reflect(widget = Color)]
    pub color: [f32; 3],
    #[reflect(units = "lm", range = 0.0..)]
    pub intensity: f32,
    #[reflect(units = "m", range = 0.0..)]
    pub radius: f32,
    #[reflect(units = "m", range = 0.01..)]
    pub range: f32,
    #[reflect(units = "°", range = 0.0..=90.0)]
    pub inner_angle: f32,
    #[reflect(units = "°", range = 0.0..=90.0)]
    pub outer_angle: f32,
}

impl Default for SpotLight {
    fn default() -> Self {
        Self {
            color: [1.0; 3],
            intensity: 800.0,
            radius: 0.05,
            range: 10.0,
            inner_angle: 0.0,
            outer_angle: 44.0,
        }
    }
}

/// Turns the entity about `axis` at `speed`, from the rotation it had when the component was
/// added.
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(name = "marlin_framework::RotatingMovement", default)]
pub struct RotatingMovement {
    pub axis: Vec3,
    #[reflect(units = "°/s")]
    pub speed: f32,
}

impl Default for RotatingMovement {
    fn default() -> Self {
        Self {
            axis: Vec3::Y,
            speed: 45.0,
        }
    }
}

/// The rotation a [`RotatingMovement`] turns from (runtime).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RotatingBase(pub Quat);

/// Free flight: look with the right mouse button held (or the right stick), move with WASD/QE
/// (or the left stick and triggers), Shift to go faster.
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(name = "marlin_framework::SpectatorMovement", default)]
pub struct SpectatorMovement {
    #[reflect(units = "m/s", range = 0.0..)]
    pub speed: f32,
}

impl Default for SpectatorMovement {
    fn default() -> Self {
        Self { speed: 10.0 }
    }
}

/// Where a [`SpectatorMovement`] looks, in radians (runtime): yaw about +Y, then pitch.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SpectatorLook {
    pub yaw: f32,
    pub pitch: f32,
}

/// Lighting and background for the level: on one entity, conventionally named
/// `LevelSettings` (see `marlin_render::Environment`).
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(name = "marlin_framework::LevelSettings", default)]
pub struct LevelSettings {
    /// The direction the sun's light travels.
    pub light_direction: [f32; 3],
    /// The sun's linear color.
    #[reflect(widget = Color)]
    pub light_color: [f32; 3],
    /// The sun's illuminance (noon: 100 000).
    #[reflect(units = "lx")]
    pub sun_illuminance: f32,
    /// Uniform sky light: the luminance of the sky.
    #[reflect(units = "nt")]
    pub ambient: [f32; 3],
    /// The background: linear RGB luminance in nits, and alpha.
    pub clear_color: [f32; 4],
    /// The sun's angular diameter: how soft its shadows are.
    #[reflect(units = "°")]
    pub sun_angular_diameter: f32,
}

impl Default for LevelSettings {
    fn default() -> Self {
        let e = marlin_render::Environment::default();
        Self {
            light_direction: [
                e.light_direction.x,
                e.light_direction.y,
                e.light_direction.z,
            ],
            light_color: e.light_color,
            sun_illuminance: e.sun_illuminance,
            ambient: e.ambient,
            clear_color: e.clear_color,
            sun_angular_diameter: marlin_render::SUN_ANGULAR_DIAMETER_DEGREES,
        }
    }
}

impl LevelSettings {
    pub fn to_render(&self) -> marlin_render::Environment {
        let [x, y, z] = self.light_direction;
        marlin_render::Environment {
            light_direction: Vec3::new(x, y, z),
            light_color: self.light_color,
            sun_illuminance: self.sun_illuminance,
            ambient: self.ambient,
            clear_color: self.clear_color,
            sun_angular_diameter: self.sun_angular_diameter.to_radians(),
        }
    }
}

/// The component types of an engine build: the world's and these. Cookers and worlds use the
/// same registry, so data means the same in both.
pub fn engine_components() -> Arc<ComponentRegistry> {
    let mut r = ComponentRegistry::new();
    r.add::<MeshRenderer>(ComponentFlags::SAVED)
        .add::<Camera>(ComponentFlags::SAVED)
        .add::<PointLight>(ComponentFlags::SAVED)
        .add::<SpotLight>(ComponentFlags::SAVED)
        .add::<RotatingMovement>(ComponentFlags::SAVED)
        .add::<SpectatorMovement>(ComponentFlags::SAVED)
        .add::<LevelSettings>(ComponentFlags::SAVED)
        .add_runtime::<RotatingBase>(marlin_ecs::StorageType::Table)
        .add_runtime::<SpectatorLook>(marlin_ecs::StorageType::Table);
    Arc::new(r)
}
