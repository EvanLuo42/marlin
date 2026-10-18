//! Scene rendering: materials, lighting, shadows and post-processing.
//!
//! So far a forward renderer for opaque meshes: entities with a [`Transform`] and a
//! [`MeshRenderer`] are drawn from the first entity with a [`Transform`] and a [`Camera`], lit by
//! one directional light plus ambient. Draws are instanced per mesh; vertices are pulled from
//! bindless buffers. See `docs/design/0002-shader-interface.md` for the shader side.
//!
//! The renderer declares its work into the frame's render graph ([`Renderer::add_passes`]):
//! the depth buffer is a transient, the frame and instance constants go through the frame's
//! upload memory, and the caller gives the color target, a swapchain image or an offscreen one.
//! The post chain ([`PostProcess`]) takes the HDR scene to the output: bloom, a luminance
//! histogram and auto-exposure on the compute queue, and a filmic tonemap.
//!
//! [`Transform`]: marlin_math::Transform
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod asset;
mod mesh;
mod post;
mod renderer;

pub use mesh::{MeshData, Vertex, cube};
pub use post::{HDR_FORMAT, PostOutput, PostProcess, PostSettings, PostShaders};
pub use renderer::{
    Camera, Environment, FORWARD_PUSH_BYTES, ForwardShaders, MeshHandle, MeshRenderer, RenderStats,
    Renderer, RendererDesc, Scene, SceneInstance,
};
