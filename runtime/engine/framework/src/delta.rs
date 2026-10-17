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
use marlin_render::{Camera, Environment, MeshHandle, RenderStats, Renderer, Scene, SceneInstance};
use marlin_rhi::{CommandList, Device, RenderTargetView};

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
    pub uploads: Vec<MeshUpload>,
    /// Meshes no instance uses any more: their GPU copies may go.
    pub releases: Vec<Uuid>,
    pub camera: Option<(Transform, Camera)>,
    pub environment: Environment,
    pub instances: Vec<RenderInstance>,
}

/// The renderer and the GPU copies of meshes, by asset.
pub struct RenderSide {
    pub renderer: Renderer,
    meshes: HashMap<Uuid, (MeshHandle, u32)>,
    /// Meshes replaced or released, freed once the frames that drew them are done.
    retired: Vec<(u64, MeshHandle)>,
    frames_in_flight: u64,
    frame: u64,
    scene: Scene,
}

impl RenderSide {
    pub fn new(renderer: Renderer, frames_in_flight: usize) -> Self {
        Self {
            renderer,
            meshes: HashMap::new(),
            retired: Vec::new(),
            frames_in_flight: frames_in_flight as u64,
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
        // frames began (each begin waits for the slot's previous frame).
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

    /// Records the last applied delta's scene into `list`.
    ///
    /// # Safety
    /// As [`Renderer::render`].
    pub unsafe fn record(
        &mut self,
        device: &Device,
        list: &mut CommandList,
        slot: usize,
        target: &RenderTargetView,
    ) -> marlin_rhi::Result<RenderStats> {
        // SAFETY: forwarded.
        unsafe {
            self.renderer
                .render_scene(device, list, slot, target, &self.scene)
        }
    }
}
