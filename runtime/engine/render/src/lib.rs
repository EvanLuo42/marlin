//! Scene rendering: materials, lighting, shadows and post-processing.
//!
//! The renderer draws a [`RenderScene`]: the engine's own copy of what is visible, apart from the
//! ECS, changed only through [`SceneUpdates`] that extract writes each frame (0009 §1). Its
//! tables of meshes, instances and lights live on the GPU as well, the GPU Scene, which one
//! scatter pass a frame brings up to date by writing just the rows that changed.
//!
//! So far a forward renderer for opaque meshes, lit by one directional light plus ambient. Draws
//! are instanced per mesh; vertices are pulled from bindless buffers, transforms and colors read
//! from the GPU Scene. See `docs/design/0002-shader-interface.md` for the shader side.
//!
//! The renderer declares its work into the frame's render graph ([`Renderer::add_passes`]):
//! the depth buffer is a transient, the frame constants and instance lists go through the
//! frame's upload memory, and the caller gives the color target, a swapchain image or an
//! offscreen one. The post chain ([`PostProcess`]) takes the HDR scene to the output: bloom, a
//! luminance histogram and auto-exposure on the compute queue, and a filmic tonemap.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod asset;
pub mod geometry;
mod post;
mod renderer;
pub mod scene;

pub use post::{HDR_FORMAT, PostOutput, PostProcess, PostSettings, PostShaders};
pub use renderer::{
    Camera, Environment, FORWARD_PUSH_BYTES, ForwardShaders, MeshHandle, RenderStats, Renderer,
    RendererDesc,
};
pub use scene::{GpuScene, RenderScene, ScatterShader, SceneIds, SceneUpdates};
