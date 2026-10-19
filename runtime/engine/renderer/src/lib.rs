//! The render module: worlds drawn by the renderer (docs/design/0019-engine-framework-and-world.md
//! §7, §8).
//!
//! `marlin-render` has the scene and its passes, `marlin-material` the materials; this crate
//! puts them to work for worlds:
//!
//! - [`RenderModule`]: the render components ([`MeshRenderer`], [`Camera`], [`PointLight`],
//!   [`SpotLight`], [`LevelSettings`]), the extract system that turns a world's changes into scene
//!   updates and its cameras into views, the loaders of what it draws, and its engine assets
//!   ([`RenderShaders`]).
//! - [`RenderDelta`]: one world frame for the renderer, which hosts [`take_delta`] from the
//!   world after its frame.
//! - [`WorldRenderer`]: a world's render side (the render scene, the GPU meshes and the passes),
//!   owned by the host so it draws frame N while the world simulates N+1.
//! - [`Offscreen`]: drawing into a texture and reading it back.
//! - [`settings`]: the quality cvars, read into the world renderer each frame.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod assets;
mod components;
mod delta;
mod extract;
mod meshes;
mod module;
mod offscreen;
pub mod settings;
mod world_renderer;

pub use assets::{
    DEFAULT_BAKE, DEFAULT_RESOLVE, ENGINE_SHADERS, PAIRING_PATTERN, RenderShaders, ShaderError,
    load_default_bake, load_default_resolve, load_engine_shaders, load_pairing_pattern,
};
pub use components::{Camera, LevelSettings, MeshRenderer, PointLight, SpotLight, ViewTarget};
pub use delta::{MeshUpload, RenderDelta, RenderView, ViewId};
pub use extract::{
    RENDER_ORIGIN, RenderExtract, extract, instance_count, point_light_desc, scene_ids,
    spot_light_desc, take_delta,
};
/// What a world's render scene is keyed by: [`scene_ids`] of a world, for [`WorldRenderer::new`].
pub use marlin_render::SceneIds;
pub use meshes::{LoadedMesh, StreamedMesh, load_mesh};
pub use module::RenderModule;
pub use offscreen::{OFFSCREEN_FORMAT, Offscreen};
pub use world_renderer::{WorldRenderer, WorldRendererError};
