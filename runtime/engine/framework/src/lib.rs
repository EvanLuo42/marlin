//! The game framework: startup, the frame loop and the engine's subsystems wired together
//! (`docs/design/0003-asset-pipeline.md` §9).
//!
//! - [`boot`]: `boot.toml` cvars → mounted content (packages, the cook service) and the entry
//!   level.
//! - [`Engine`]: the simulation side: the asset server, the world, the level and the engine's
//!   systems. Each frame returns a [`RenderDelta`].
//! - [`RenderSide`]: the renderer and GPU copies of meshes, fed by deltas. The two sides share no
//!   state, so [`run`] simulates frame N+1 on a job while rendering frame N.
//! - [`level`]: the level asset; [`loaders`]: what each cooked type becomes at runtime.
//! - [`Offscreen`]: rendering into a texture and reading it back.
//! - [`shaders`]: the engine's cooked shader library, found by package name.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod boot;
mod delta;
mod engine;
pub mod level;
pub mod loaders;
mod offscreen;
mod run;
pub mod shaders;

pub use boot::{Boot, BootError, ContentSettings, boot, mount, register_cvars};
pub use delta::{MeshUpload, RenderDelta, RenderInstance, RenderSide};
pub use engine::{Engine, EngineError, FlyCamera, FromLevel, Spin, StaticMesh};
pub use level::{Component, Level, LevelEntity, LevelEnvironment, TransformDesc};
pub use offscreen::{OFFSCREEN_FORMAT, Offscreen};
pub use run::{RunError, run};
pub use shaders::{ShaderError, forward_shaders, load_engine_shaders};
