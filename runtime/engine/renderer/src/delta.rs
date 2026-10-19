//! What a world's simulation hands its renderer each frame (0008 §3.1, 0019 §7, §8).
//!
//! The simulation never touches the GPU. Each frame the render module builds an immutable
//! [`RenderDelta`]: the scene's changes since the last one as [`SceneUpdates`] (extract,
//! 0009 §1.2), the meshes to upload (new, or replaced by a reload) and the frame's views. Rows
//! are named by handles the simulation reserves from the render scene's allocators
//! ([`SceneIds`](marlin_render::SceneIds)), so it never waits for the render side.

use alloc::vec::Vec;

use marlin_math::Transform;
use marlin_render::scene::MeshId;
use marlin_render::{Camera, SceneUpdates};

use crate::components::ViewTarget;
use crate::meshes::LoadedMesh;

/// A mesh the render side should have, at a version.
#[derive(Clone, Debug)]
pub struct MeshUpload {
    pub mesh: MeshId,
    /// The asset's reload count: a higher one replaces the GPU copy.
    pub version: u32,
    /// The cooked mesh, uploaded as it is.
    pub cooked: LoadedMesh,
}

/// A view's stable identity: the key of its history (exposure, TSR, denoising, ReSTIR). Below
/// 2^63: the renderer keys a view's second signal with the top bit.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub struct ViewId(pub u64);

impl ViewId {
    /// A view id from any 64 bits.
    pub const fn new(bits: u64) -> Self {
        Self(bits & !(1 << 63))
    }
}

/// One view of the frame: a camera, where it is and where it draws.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RenderView {
    pub id: ViewId,
    pub target: ViewTarget,
    pub priority: i32,
    /// Relative to the render origin.
    pub transform: Transform,
    pub camera: Camera,
}

/// One frame of the simulation, for the renderer.
#[derive(Clone, Debug, Default)]
pub struct RenderDelta {
    /// The simulation frame it was taken at.
    pub frame: u64,
    /// The frame's time step, seconds: how far exposure adapts.
    pub dt: f32,
    /// What changed in the scene since the last delta. A mesh's `Remove` also frees its GPU
    /// copy.
    pub updates: SceneUpdates,
    /// Geometry for meshes the updates added, or a newer version of it.
    pub uploads: Vec<MeshUpload>,
    /// Every camera's view, highest priority first.
    pub views: Vec<RenderView>,
}

impl RenderDelta {
    /// The view drawn into `target`: its highest-priority one.
    pub fn view(&self, target: ViewTarget) -> Option<&RenderView> {
        self.views.iter().find(|v| v.target == target)
    }
}
