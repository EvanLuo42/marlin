//! The render module's components, in authored units (0019 §5.1, §7, §8).
//!
//! Each is one reflected type, saved in levels as written (`MeshRenderer { Mesh = <…> }`),
//! with its defaults in `Default`. Conversions to what the renderer wants (lumens to candela,
//! degrees to cosines, world positions to the render origin) happen in extract
//! ([`crate::extract`]).
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

use marlin_asset::Link;
use marlin_math::Vec3;
use marlin_reflect::Reflect;
use marlin_render::asset::Mesh;

/// Draws a mesh at the entity's transform.
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(name = "marlin_renderer::MeshRenderer", default)]
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

/// Where a [`Camera`] draws (0019 §8).
#[derive(Clone, Copy, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "marlin_renderer::ViewTarget", default)]
pub enum ViewTarget {
    /// A local player's view: the host decides which window, and which part of it (split
    /// screen).
    LocalPlayer(u8),
    /// The main view of the game's window: what a single-player game shows.
    #[default]
    Main,
    /// An offscreen target the host or a tool names (a minimap, a monitor, a capture).
    Offscreen(u32),
}

/// A view into the world. Every camera is a view in the frame's list; hosts draw the
/// highest-priority view of each target.
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(name = "marlin_renderer::Camera", default)]
pub struct Camera {
    pub target: ViewTarget,
    /// Among cameras with the same target, the highest is drawn.
    pub priority: i32,
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
            target: ViewTarget::Main,
            priority: 0,
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
#[reflect(name = "marlin_renderer::PointLight", default)]
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
#[reflect(name = "marlin_renderer::SpotLight", default)]
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

/// Lighting and background for the level: on one entity, conventionally named
/// `LevelSettings` (see `marlin_render::Environment`).
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(name = "marlin_renderer::LevelSettings", default)]
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
