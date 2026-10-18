//! What the simulation hands the renderer each frame, and the render side that consumes it.
//!
//! The simulation never touches the GPU. Each frame it builds an immutable [`RenderDelta`]:
//! meshes to upload (new, or replaced by a reload), the camera, the environment and every
//! instance, naming meshes by asset UUID. The [`RenderSide`] owns the renderer and the GPU
//! meshes: it applies the delta and draws. So the two run on different jobs, the simulation one
//! frame ahead. Instances are sent whole for now; the delta narrows to what changed once a
//! GPU Scene keeps the rest.

use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_asset::Uuid;
use marlin_containers::{HashMap, HashMapExt};
use marlin_math::Transform;
use marlin_render::{
    Camera, Environment, HDR_FORMAT, MeshHandle, PostProcess, RenderStats, Renderer, Scene,
    SceneInstance,
};
use marlin_render_graph::{FRAMES_IN_FLIGHT, RenderGraph, TextureDesc, TextureHandle};
use marlin_rhi::{ClearValue, Device};

/// The view whose exposure the game's frames adapt.
const MAIN_VIEW: u64 = 0;

use crate::loaders::MeshGeometry;

/// A mesh the render side should have, at a version.
#[derive(Clone, Debug)]
pub struct MeshUpload {
    pub mesh: Uuid,
    /// The asset's reload count: a higher one replaces the GPU copy.
    pub version: u32,
    pub geometry: Arc<MeshGeometry>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RenderInstance {
    pub mesh: Uuid,
    pub transform: Transform,
    pub color: [f32; 4],
}

/// One frame of the simulation, for the renderer.
#[derive(Clone, Debug, Default)]
pub struct RenderDelta {
    /// The simulation frame it was taken at.
    pub frame: u64,
    /// The frame's time step, seconds: how far exposure adapts.
    pub dt: f32,
    pub uploads: Vec<MeshUpload>,
    /// Meshes no instance uses any more: their GPU copies may go.
    pub releases: Vec<Uuid>,
    pub camera: Option<(Transform, Camera)>,
    pub environment: Environment,
    pub instances: Vec<RenderInstance>,
}

/// The renderer and the GPU copies of meshes, by asset.
pub struct RenderSide {
    /// Draws into an [`HDR_FORMAT`] scene, which `post` tonemaps into the output.
    pub renderer: Renderer,
    pub post: PostProcess,
    /// The last applied delta's time step.
    dt: f32,
    meshes: HashMap<Uuid, (MeshHandle, u32)>,
    /// Meshes replaced or released, freed once the frames that drew them are done.
    retired: Vec<(u64, MeshHandle)>,
    frames_in_flight: u64,
    frame: u64,
    scene: Scene,
}

impl RenderSide {
    /// `renderer` draws in [`HDR_FORMAT`]; `post` writes the output's format.
    pub fn new(renderer: Renderer, post: PostProcess) -> Self {
        Self {
            renderer,
            post,
            dt: 0.0,
            meshes: HashMap::new(),
            retired: Vec::new(),
            frames_in_flight: FRAMES_IN_FLIGHT as u64,
            frame: 0,
            scene: Scene::default(),
        }
    }

    /// GPU meshes held, by asset.
    pub fn mesh_count(&self) -> usize {
        self.meshes.len()
    }

    /// Takes in `delta`'s uploads and releases. Uploads block until their copies finish.
    pub fn apply(&mut self, device: &Device, delta: &RenderDelta) -> marlin_rhi::Result<()> {
        self.frame += 1;
        for upload in &delta.uploads {
            if self
                .meshes
                .get(&upload.mesh)
                .is_some_and(|&(_, v)| v >= upload.version)
            {
                continue;
            }
            let handle = self.renderer.upload_mesh(device, &upload.geometry.data())?;
            if let Some((old, _)) = self.meshes.insert(upload.mesh, (handle, upload.version)) {
                self.retired.push((self.frame, old));
            }
        }
        for uuid in &delta.releases {
            if let Some((old, _)) = self.meshes.remove(uuid) {
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
        self.renderer.environment = delta.environment;
        self.dt = delta.dt;
        self.scene.camera = delta.camera;
        self.scene.instances.clear();
        for i in &delta.instances {
            if let Some(&(mesh, _)) = self.meshes.get(&i.mesh) {
                self.scene.instances.push(SceneInstance {
                    mesh,
                    transform: i.transform,
                    color: i.color,
                });
            }
        }
        Ok(())
    }

    /// Declares the last applied delta's frame into `g`: the scene into a transient
    /// [`HDR_FORMAT`] target the size of `target` ([`Renderer::add_passes`]), then the post chain
    /// into `target` ([`PostProcess::add_passes`]). Needs a graph with a state table.
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        target: TextureHandle<'g>,
    ) -> (TextureHandle<'g>, RenderStats) {
        let size = *g.texture_desc(target);
        let scene = g.create_texture(
            "Scene",
            TextureDesc::new_2d(size.width, size.height, HDR_FORMAT)
                .with_clear(ClearValue::Color(self.renderer.environment.clear_color)),
        );
        let (scene, stats) = self.renderer.add_passes(g, scene, &self.scene);
        let out = self.post.add_passes(g, scene, target, MAIN_VIEW, self.dt);
        (out.output, stats)
    }
}
