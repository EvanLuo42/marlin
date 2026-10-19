//! The game framework: startup, the frame loop and the engine's subsystems wired together
//! (`docs/design/0003-asset-pipeline.md` §9).
//!
//! - [`boot`]: `boot.toml` cvars → mounted content (packages, the cook service) and the entry
//!   level.
//! - [`Engine`]: the simulation side: the asset server, the world, the level and the engine's
//!   systems. Each frame returns a [`RenderDelta`].
//! - [`RenderSide`]: the renderer, the render scene and GPU copies of meshes, fed by deltas. The
//!   two sides share only the scene's row allocators, so [`run`] simulates frame N+1 on a job
//!   while rendering frame N.
//! - [`components`]: the components levels place, and the engine's component registry;
//!   [`loaders`]: what each cooked type becomes at runtime. Levels themselves are
//!   `marlin_world`'s (0019 §5).
//! - [`Offscreen`]: rendering into a texture and reading it back.
//! - [`shaders`]: the engine's cooked shader library, found by package name.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod boot;
pub mod components;
mod delta;
mod dev;
mod engine;
#[cfg(not(marlin_build = "shipping"))]
pub mod hud;
pub mod loaders;
mod offscreen;
pub mod quality;
mod run;
pub mod scalability;
pub mod shaders;

pub use boot::{Boot, BootError, ContentSettings, boot, mount, register_cvars};
pub use components::{
    Camera, LevelSettings, MeshRenderer, PointLight, RotatingMovement, SpectatorMovement,
    SpotLight, engine_components,
};
pub use delta::{MeshUpload, RenderDelta, RenderSide, RenderSideError};
pub use engine::{
    Engine, EngineError, FrameInput, RenderExtract, point_light_desc, spot_light_desc,
};
pub use offscreen::{OFFSCREEN_FORMAT, Offscreen};
pub use run::{RunError, run};
pub use shaders::{
    DEFAULT_BAKE, DEFAULT_RESOLVE, ENGINE_SHADERS, PAIRING_PATTERN, ShaderError, UI_FONT,
    load_default_bake, load_default_resolve, load_engine_shaders, load_pairing_pattern,
    load_ui_font,
};
