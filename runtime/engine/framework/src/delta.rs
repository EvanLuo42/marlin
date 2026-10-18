//! What the simulation hands the renderer each frame, and the render side that consumes it.
//!
//! The simulation never touches the GPU. Each frame it builds an immutable [`RenderDelta`]: the
//! scene's changes since the last one as [`SceneUpdates`] (extract, 0009 §1.2), the meshes to
//! upload (new, or replaced by a reload) and the camera. Rows are named by handles the
//! simulation reserves from the render scene's allocators ([`SceneIds`]), so it never waits for
//! the render side. The [`RenderSide`] owns the passes, the [`RenderScene`] and the GPU meshes:
//! it uploads, applies the updates and draws. So the two run on different jobs, the simulation
//! one frame ahead.

use alloc::vec::Vec;

use marlin_containers::{HashMap, HashMapExt};
use marlin_material::{DEFAULT_PERMUTATION, Materials};
use marlin_math::Transform;
use marlin_render::scene::{MeshId, MeshUpdate};
use marlin_render::{
    Camera, Lighting, MeshHandle, Meshes, PostProcess, RenderScene, ScatterShader, SceneIds,
    SceneUpdates, Visibility,
};
use marlin_render_graph::{FRAMES_IN_FLIGHT, RenderGraph, TextureHandle};
use marlin_rhi::{Device, Format};
use marlin_shader::asset::ShaderLibrary;

/// The view whose exposure the game's frames adapt.
const MAIN_VIEW: u64 = 0;

use crate::loaders::LoadedMesh;
use crate::shaders::{
    ShaderError, lighting_shader, material_shaders, post_shaders, resolve_shader, scatter_shader,
    visibility_shaders,
};

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

/// The render scene, the GPU copies of its meshes and the passes that draw it.
pub struct RenderSide {
    /// The resident meshes.
    pub meshes: Meshes,
    pub visibility: Visibility,
    pub materials: Materials,
    pub lighting: Lighting,
    /// Tonemaps the [`HDR_FORMAT`](marlin_render::HDR_FORMAT) scene into the output.
    pub post: PostProcess,
    pub scene: RenderScene,
    scatter: ScatterShader,
    /// The last applied delta's time step and camera, and the camera before it.
    dt: f32,
    camera: Option<(Transform, Camera)>,
    previous: Option<(Transform, Camera)>,
    uploaded: HashMap<MeshId, (MeshHandle, u32)>,
    /// Meshes replaced or released, freed once the frames that drew them are done.
    retired: Vec<(u64, MeshHandle)>,
    frames_in_flight: u64,
    frame: u64,
}

impl RenderSide {
    /// The passes made with the engine's shaders (`engine`, the `Marlin.Engine` library) and
    /// the default material's resolve permutation (`default_resolve`, `DEFAULT_RESOLVE`);
    /// the post chain writes `output`. The scene's rows come from `ids`, the allocators the
    /// simulation reserves them from.
    pub fn new(
        device: &Device,
        engine: &ShaderLibrary,
        default_resolve: &ShaderLibrary,
        output: Format,
        ids: SceneIds,
    ) -> Result<Self, RenderSideError> {
        let mut materials = Materials::new(device, &material_shaders(engine)?)?;
        let default = materials.register(
            device,
            "Marlin.Engine.DefaultMaterial",
            resolve_shader(default_resolve)?,
        )?;
        debug_assert_eq!(default, DEFAULT_PERMUTATION);
        Ok(Self {
            meshes: Meshes::new(),
            visibility: Visibility::new(device, &visibility_shaders(engine)?)?,
            materials,
            lighting: Lighting::new(device, lighting_shader(engine)?)?,
            post: PostProcess::new(device, &post_shaders(engine)?, output)?,
            scene: RenderScene::with_ids(ids),
            scatter: ScatterShader::new(device, scatter_shader(engine)?)?,
            dt: 0.0,
            camera: None,
            previous: None,
            uploaded: HashMap::new(),
            retired: Vec::new(),
            frames_in_flight: FRAMES_IN_FLIGHT as u64,
            frame: 0,
        })
    }

    /// GPU meshes held.
    pub fn mesh_count(&self) -> usize {
        self.uploaded.len()
    }

    /// Takes in `delta`: uploads its meshes (blocking until the copies finish), frees removed
    /// ones and applies its updates to the scene.
    pub fn apply(&mut self, device: &Device, delta: &RenderDelta) -> marlin_rhi::Result<()> {
        self.frame += 1;
        let mut resident = SceneUpdates::default();
        for upload in &delta.uploads {
            if self
                .uploaded
                .get(&upload.mesh)
                .is_some_and(|&(_, v)| v >= upload.version)
            {
                continue;
            }
            let cooked = &upload.cooked.0;
            let handle = self
                .meshes
                .upload_mesh(device, &cooked.view(), &cooked.pages())?;
            if let Some(m) = self.meshes.resident_mesh(handle) {
                resident.meshes.push(upload.mesh, MeshUpdate::Resident(m));
            }
            if let Some((old, _)) = self.uploaded.insert(upload.mesh, (handle, upload.version)) {
                self.retired.push((self.frame, old));
            }
        }
        for (id, update) in delta.updates.meshes.iter() {
            if *update == MeshUpdate::Remove
                && let Some((old, _)) = self.uploaded.remove(id)
            {
                self.retired.push((self.frame, old));
            }
        }
        // Frames recorded before a mesh retired have finished once `frames_in_flight` more
        // frames began (each frame waits for its slot's previous frame).
        let (frame, depth) = (self.frame, self.frames_in_flight);
        let meshes = &mut self.meshes;
        self.retired.retain(|&(at, handle)| {
            if frame > at + depth {
                // SAFETY: see above.
                unsafe { meshes.release_mesh(handle) };
                false
            } else {
                true
            }
        });
        // The delta's adds come first: the residency of a mesh added this frame applies to it.
        self.scene.apply_all([&delta.updates, &resident]);
        self.dt = delta.dt;
        self.previous = self.camera;
        self.camera = delta.camera;
        Ok(())
    }

    /// Declares the last applied delta's frame into `g` (0008 §3): the GPU Scene's scatter, the
    /// visibility buffer and the material buffer of the camera's view, direct lighting into an
    /// [`HDR_FORMAT`](marlin_render::HDR_FORMAT) scene the size of `target`, then the post chain into `target`. Without a
    /// camera the scene is the clear color. Needs a graph with a state table.
    pub fn add_passes<'g>(
        &'g mut self,
        g: &mut RenderGraph<'g>,
        target: TextureHandle<'g>,
    ) -> TextureHandle<'g> {
        let size = *g.texture_desc(target);
        let (width, height) = (size.width, size.height);
        let gpu = self.scene.add_passes(g, &self.scatter);
        let material = self.camera.map(|camera| {
            let vis = self.visibility.add_passes(
                g,
                &self.meshes,
                &self.scene,
                gpu,
                camera,
                width,
                height,
            );
            self.materials
                .add_passes(g, gpu, &vis, camera, self.previous, width, height)
                .material
        });
        let env = *self.scene.environment();
        let scene = self.lighting.add_passes(g, material, &env, width, height);
        self.post
            .add_passes(g, scene, target, MAIN_VIEW, self.dt)
            .output
    }
}

/// Why a [`RenderSide`] could not be made.
#[derive(Debug, marlin_error::Error)]
pub enum RenderSideError {
    #[error("engine shaders")]
    Shaders(#[from] ShaderError),
    #[error("gpu")]
    Gpu(#[from] marlin_rhi::Error),
}
