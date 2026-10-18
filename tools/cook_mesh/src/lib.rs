//! glTF content for the cook pipeline: meshes, skeletons and animation clips.
//!
//! - [`import`]: glTF 2.0 geometry, skins and animations into plain arrays, every byte read
//!   through the cook context's declared sources.
//! - [`simplify`]: quadric edge-collapse simplification with attribute costs, seams kept and
//!   borders locked, for LOD chains.
//! - [`optimize`]: welding, vertex cache order (Forsyth) and vertex fetch order.
//! - [`cluster`]: clusters of up to 128 triangles and 128 vertices for the cluster DAG.
//! - [`meshlet`]: meshlets of up to 64 vertices and 124 triangles with bounding spheres and
//!   normal cones.
//! - [`page`]: cluster pages of at most 128 KiB, the page table and the group hierarchy.
//! - [`quantize`]: cluster vertex streams: positions over the mesh bounds, octahedral frames,
//!   UVs over each cluster's range.
//! - [`tangent`]: tangent frames for meshes imported without them.
//! - [`MeshCooker`], [`SkeletonCooker`], [`AnimationCooker`]: the cookers for
//!   `marlin_render::asset::Mesh` and `marlin_anim::asset::{Skeleton, AnimationClip}`.
//!
//! Every algorithm here is our own (0003); the `gltf` crate only parses the file.

mod build;
pub mod cluster;
mod cooker;
pub mod dag;
pub mod import;
mod math;
pub mod meshlet;
pub mod optimize;
pub mod page;
pub mod quantize;
pub mod simplify;
pub mod tangent;

pub use build::{Built, build};
pub use cooker::{AnimationCooker, MeshCooker, SkeletonCooker};
