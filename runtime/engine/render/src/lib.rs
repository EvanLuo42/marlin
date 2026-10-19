//! Scene rendering: materials, lighting, shadows and post-processing.
//!
//! The renderer draws a [`RenderScene`]: the engine's own copy of what is visible, apart from the
//! ECS, changed only through [`SceneUpdates`] that extract writes each frame (0009 §1). Its
//! tables of meshes, instances and lights live on the GPU as well, the GPU Scene, which one
//! scatter pass a frame brings up to date by writing just the rows that changed.
//!
//! A frame (0008 §3), each step declared into the frame's render graph:
//! - the GPU Scene's scatter ([`RenderScene::add_passes`]);
//! - the visibility passes ([`Visibility`]): instance culling, the cluster DAG's LOD cut and
//!   cluster culling on the GPU, clusters drawn by a mesh shader into a 64-bit visibility
//!   buffer;
//! - the material passes (the `marlin-material` crate): the visibility buffer's pixels binned
//!   by material and resolved into the material buffer;
//! - direct lighting ([`Lighting`]) into an HDR scene;
//! - the post chain ([`PostProcess`]): bloom, a luminance histogram and auto-exposure on the
//!   compute queue, and a filmic tonemap into the output.
//!
//! Meshes stay resident as their cooked cluster DAG pages in a page pool ([`Meshes`],
//! [`geometry`]). See
//! `docs/design/0002-shader-interface.md` for the shader side.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod asset;
pub mod geometry;
pub mod lighting;
mod meshes;
mod post;
pub mod scene;
pub mod shadows;
pub mod visibility;

pub use lighting::Lighting;
pub use meshes::{
    Camera, Environment, Feedback, MAX_POOL_SLOTS, MeshData, MeshHandle, Meshes, PagePool,
    PagePoolDesc, StreamingShaders, Uploads,
};
pub use post::{HDR_FORMAT, PostOutput, PostProcess, PostSettings, PostShaders};
pub use scene::{GpuScene, RenderScene, ScatterShader, SceneIds, SceneUpdates};
pub use visibility::{Visibility, VisibilityOutput, VisibilityShaders};
