//! What the simulation hands the renderer each frame, and the render side that consumes it.
//!
//! The simulation never touches the GPU. Each frame it builds an immutable [`RenderDelta`]: the
//! scene's changes since the last one as [`SceneUpdates`] (extract, 0009 §1.2), the meshes to
//! upload (new, or replaced by a reload) and the camera. Rows are named by handles the
//! simulation reserves from the render scene's allocators ([`SceneIds`]), so it never waits for
//! the render side. The [`RenderSide`] owns the renderer, the [`RenderScene`] and the GPU meshes:
//! it uploads, applies the updates and draws. So the two run on different jobs, the simulation
//! one frame ahead.

use alloc::vec::Vec;

use marlin_containers::{HashMap, HashMapExt};
use marlin_math::Transform;
use marlin_render::scene::{MeshId, MeshUpdate};
use marlin_render::{
    Camera, HDR_FORMAT, MeshHandle, PostProcess, RenderScene, RenderStats, Renderer, ScatterShader,
    SceneIds, SceneUpdates,
};
use marlin_render_graph::{FRAMES_IN_FLIGHT, RenderGraph, TextureDesc, TextureHandle};
use marlin_rhi::{ClearValue, Device};

/// The view whose exposure the game's frames adapt.
const MAIN_VIEW: u64 = 0;

use crate::loaders::LoadedMesh;

/// A mesh the render side should have, at a version.
#[derive(Clone, Debug)]
pub struct MeshUpload {
    pub mesh: MeshId,
    /// The asset's reload count: a higher one replaces the GPU copy.
    pub version: u32,
    /// The cooked mesh, uploaded as it is.
    pub cooked: LoadedMesh,
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
    pub camera: Option<(Transform, Camera)>,
}

/// The renderer, the render scene and the GPU copies of its meshes.
pub struct RenderSide {
    /// Draws into an [`HDR_FORMAT`] scene, which `post` tonemaps into the output.
    pub renderer: Renderer,
    pub post: PostProcess,
    pub scene: RenderScene,
    scatter: ScatterShader,
    /// The last applied delta's time step and camera.
    dt: f32,
    camera: Option<(Transform, Camera)>,
    meshes: HashMap<MeshId, (MeshHandle, u32)>,
    /// Meshes replaced or released, freed once the frames that drew them are done.
    retired: Vec<(u64, MeshHandle)>,
    frames_in_flight: u64,
    frame: u64,
}

impl RenderSide {
    /// `renderer` draws in [`HDR_FORMAT`]; `post` writes the output's format. The scene's rows
    /// come from `ids`, the allocators the simulation reserves them from.
    pub fn new(
        renderer: Renderer,
        post: PostProcess,
        scatter: ScatterShader,
        ids: SceneIds,
    ) -> Self {
        Self {
            renderer,
            post,
            scene: RenderScene::with_ids(ids),
            scatter,
            dt: 0.0,
            camera: None,
            meshes: HashMap::new(),
            retired: Vec::new(),
            frames_in_flight: FRAMES_IN_FLIGHT as u64,
            frame: 0,
        }
    }

    /// GPU meshes held.
    pub fn mesh_count(&self) -> usize {
        self.meshes.len()
    }

    /// Takes in `delta`: uploads its meshes (blocking until the copies finish), frees removed
    /// ones and applies its updates to the scene.
    pub fn apply(&mut self, device: &Device, delta: &RenderDelta) -> marlin_rhi::Result<()> {
        self.frame += 1;
        let mut resident = SceneUpdates::default();
        for upload in &delta.uploads {
            if self
                .meshes
                .get(&upload.mesh)
                .is_some_and(|&(_, v)| v >= upload.version)
            {
                continue;
            }
            let cooked = &upload.cooked.0;
            let handle = self
                .renderer
                .upload_mesh(device, &cooked.view(), &cooked.pages())?;
            if let Some(m) = self.renderer.resident_mesh(handle) {
                resident.meshes.push(upload.mesh, MeshUpdate::Resident(m));
            }
            if let Some((old, _)) = self.meshes.insert(upload.mesh, (handle, upload.version)) {
                self.retired.push((self.frame, old));
            }
        }
        for (id, update) in delta.updates.meshes.iter() {
            if *update == MeshUpdate::Remove
                && let Some((old, _)) = self.meshes.remove(id)
            {
                self.retired.push((self.frame, old));
            }
        }
        // Frames recorded before a mesh retired have finished once `frames_in_flight` more
        // frames began (each frame waits for its slot's previous frame).
        let (frame, depth) = (self.frame, self.frames_in_flight);
        let renderer = &mut self.renderer;
        self.retired.retain(|&(at, handle)| {
            if frame > at + depth {
                // SAFETY: see above.
                unsafe { renderer.release_mesh(handle) };
                false
            } else {
                true
            }
        });
        // The delta's adds come first: the residency of a mesh added this frame applies to it.
        self.scene.apply_all([&delta.updates, &resident]);
        self.dt = delta.dt;
        self.camera = delta.camera;
        Ok(())
    }

    /// Declares the last applied delta's frame into `g`: the GPU Scene's scatter, the scene into
    /// a transient [`HDR_FORMAT`] target the size of `target` ([`Renderer::add_passes`]), then
    /// the post chain into `target` ([`PostProcess::add_passes`]). Needs a graph with a state
    /// table.
    pub fn add_passes<'g>(
        &'g mut self,
        g: &mut RenderGraph<'g>,
        target: TextureHandle<'g>,
    ) -> (TextureHandle<'g>, RenderStats) {
        let size = *g.texture_desc(target);
        let gpu = self.scene.add_passes(g, &self.scatter);
        let scene = g.create_texture(
            "Scene",
            TextureDesc::new_2d(size.width, size.height, HDR_FORMAT)
                .with_clear(ClearValue::Color(self.scene.environment().clear_color)),
        );
        let forward = self
            .renderer
            .add_passes(g, scene, &self.scene, gpu, self.camera);
        let out = self
            .post
            .add_passes(g, forward.color, target, MAIN_VIEW, self.dt);
        (out.output, forward.stats)
    }
}
