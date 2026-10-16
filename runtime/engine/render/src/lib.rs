//! Scene rendering: materials, lighting, shadows and post-processing.
//!
//! So far a forward renderer for opaque meshes: entities with a [`Transform`] and a
//! [`MeshRenderer`] are drawn from the first entity with a [`Transform`] and a [`Camera`], lit by
//! one directional light plus ambient. Draws are instanced per mesh; vertices are pulled from
//! bindless buffers. See `docs/design/0002-shader-interface.md` for the shader side.
//!
//! [`Transform`]: marlin_math::Transform
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod mesh;
mod renderer;
mod shaders;

pub use mesh::{MeshData, Vertex, cube};
pub use renderer::{
    Camera, Environment, MeshHandle, MeshRenderer, RenderStats, Renderer, RendererDesc,
};
