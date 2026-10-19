//! What the simulation hands the renderer each frame, and the render side that consumes it.
//!
//! The simulation never touches the GPU. Each frame it builds an immutable [`RenderDelta`]: the
//! scene's changes since the last one as [`SceneUpdates`] (extract, 0009 §1.2), the meshes to
//! upload (new, or replaced by a reload) and the camera. Rows are named by handles the
//! simulation reserves from the render scene's allocators ([`SceneIds`]), so it never waits for
//! the render side. The [`RenderSide`] owns the passes, the [`RenderScene`] and the GPU meshes:
//! it uploads, applies the updates and draws. So the two run on different jobs, the simulation
//! one frame ahead.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_containers::{HashMap, HashMapExt};
use marlin_material::{DEFAULT_PERMUTATION, MaterialShaders, Materials, RESOLVE};
use marlin_math::{Transform, UVec2, Vec2};
use marlin_render::asset::PairingPatternView;
use marlin_render::denoise::{DenoiseInputs, DenoiseShaders, Denoiser};
use marlin_render::lights::LightTiles;
use marlin_render::raytracing::{RtShaders, Tlas};
use marlin_render::restir::{RestirDi, RestirInputs, RestirShaders};
use marlin_render::scene::{MeshId, MeshUpdate};
use marlin_render::shadows::{Vsm, VsmSettings, VsmShaders};
use marlin_render::upscale::{Tsr, TsrShaders, UpscaleInputs, UpscaleMode, Upscaler};
use marlin_render::visibility::{ViewDesc, counters};
use marlin_render::{
    Camera, Feedback, Lighting, MeshData, MeshHandle, Meshes, PagePoolDesc, PostProcess,
    PostShaders, RenderScene, ScatterShader, SceneIds, SceneUpdates, StreamingShaders, Visibility,
    VisibilityShaders,
};
use marlin_render_graph::{FRAMES_IN_FLIGHT, FrameContext, Readback, RenderGraph, TextureHandle};
use marlin_rhi::{Device, Format};
use marlin_shader::asset::ShaderLibrary;
use marlin_shader::bind::{BindError, bind_one};

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

/// The render scene, the GPU copies of its meshes and the passes that draw it.
pub struct RenderSide {
    /// The resident meshes.
    pub meshes: Meshes,
    pub visibility: Visibility,
    pub materials: Materials,
    pub lighting: Lighting,
    /// The sun's virtual shadow maps.
    pub vsm: Vsm,
    /// The ray tracing scene's TLAS (0007 §9).
    pub tlas: Tlas,
    /// The frame's presampled light tiles (0010 §2.8).
    pub light_tiles: LightTiles,
    /// Local lights: ReSTIR DI (0010 §2).
    pub restir: RestirDi,
    /// Denoises ReSTIR's light (0010 §6); `denoise` off composes it as it is.
    pub denoiser: Denoiser,
    pub denoise: bool,
    /// Resolves the view, drawn at `upscale`'s render size with a jitter a frame, to the target's
    /// size (0010 §7): TSR, at the native size by default, so it is the anti-aliasing. `None`
    /// draws at the target's size without jitter or resolve.
    pub tsr: Tsr,
    pub upscale: Option<UpscaleMode>,
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
    /// Streaming feedback not read back yet: the frame, its counters, requests and used marks.
    feedback: VecDeque<(u64, [Readback; 3])>,
    /// Whether the meshes have once finished streaming in: until then the exposure follows the
    /// scene at once, since a half-loaded scene (often only the sky) is no light to adapt from.
    loaded: bool,
}

impl RenderSide {
    /// The passes made with the engine's shaders (`engine`, the `Marlin.Engine` library), the
    /// default material's resolve permutation (`default_resolve`, `DEFAULT_RESOLVE`) and ReSTIR's
    /// pairing patterns (`pairing`, `PAIRING_PATTERN`); the post chain writes `output`. The
    /// scene's rows come from `ids`, the allocators the simulation reserves them from.
    pub fn new(
        device: &Device,
        engine: &ShaderLibrary,
        default_resolve: &ShaderLibrary,
        pairing: PairingPatternView<'_>,
        output: Format,
        ids: SceneIds,
    ) -> Result<Self, RenderSideError> {
        let mut materials = Materials::new(device, &MaterialShaders::bind(engine)?)?;
        let default = materials.register(
            device,
            "Marlin.Engine.DefaultMaterial",
            bind_one(default_resolve, &RESOLVE)?,
        )?;
        debug_assert_eq!(default, DEFAULT_PERMUTATION);
        Ok(Self {
            meshes: Meshes::new(
                device,
                &PagePoolDesc::DEFAULT,
                &StreamingShaders::bind(engine)?,
            )?,
            visibility: Visibility::new(device, &VisibilityShaders::bind(engine)?)?,
            materials,
            lighting: Lighting::new(device, bind_one(engine, &Lighting::ENTRY)?)?,
            vsm: Vsm::new(device, &VsmShaders::bind(engine)?, VsmSettings::DEFAULT)?,
            tlas: Tlas::new(device, &RtShaders::bind(engine)?)?,
            light_tiles: LightTiles::new(device, bind_one(engine, &LightTiles::ENTRY)?)?,
            restir: RestirDi::new(device, &RestirShaders::bind(engine)?, pairing)?,
            denoiser: Denoiser::new(device, &DenoiseShaders::bind(engine)?)?,
            denoise: true,
            tsr: Tsr::new(device, &TsrShaders::bind(engine)?)?,
            upscale: Some(UpscaleMode::Native),
            post: PostProcess::new(device, &PostShaders::bind(engine)?, output)?,
            scene: RenderScene::with_ids(ids),
            scatter: ScatterShader::new(device, bind_one(engine, &ScatterShader::ENTRY)?)?,
            dt: 0.0,
            camera: None,
            previous: None,
            uploaded: HashMap::new(),
            retired: Vec::new(),
            frames_in_flight: FRAMES_IN_FLIGHT as u64,
            frame: 0,
            feedback: VecDeque::new(),
            loaded: false,
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
                .upload_mesh(device, cooked.clone() as Arc<dyn MeshData>)?;
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
        // The frame's BLAS builds and the TLAS's room for the instance rows.
        self.meshes.prepare_rt(device)?;
        self.tlas.prepare(device, self.scene.instance_rows())?;
        self.dt = delta.dt;
        self.previous = self.camera;
        self.camera = delta.camera;
        Ok(())
    }

    /// Declares the last applied delta's frame into `g` (0008 §3): the GPU Scene's scatter, the
    /// visibility buffer and the material buffer of the camera's view at the upscaler's render
    /// size, jittered; direct lighting and the denoised local lights into an
    /// [`HDR_FORMAT`](marlin_render::HDR_FORMAT) scene; TSR to the size of `target`; then the post
    /// chain into `target`. Without a camera the scene is the clear color. Needs a graph with a
    /// state table.
    pub fn add_passes<'g>(
        &'g mut self,
        g: &mut RenderGraph<'g>,
        target: TextureHandle<'g>,
    ) -> TextureHandle<'g> {
        let size = *g.texture_desc(target);
        let display = UVec2::new(size.width, size.height);
        // The render size and this frame's jitter, which goes into the camera (and so into last
        // frame's camera next frame).
        let render = match (self.upscale, self.camera) {
            (Some(mode), Some(_)) => self.tsr.render_size(display, mode),
            _ => display,
        };
        if let (Some(_), Some((_, camera))) = (self.upscale, self.camera.as_mut()) {
            let jitter = self.tsr.jitter(self.frame, render, display);
            camera.jitter = [jitter.x, jitter.y];
        }
        let (width, height) = (render.x, render.y);
        if !self.loaded && !self.uploaded.is_empty() && self.meshes.streaming_idle() {
            self.loaded = true;
        }
        let dt = if self.loaded { self.dt } else { f32::INFINITY };
        let uploads = self.meshes.take_uploads();
        let Self {
            meshes,
            visibility,
            materials,
            lighting,
            vsm,
            post,
            tlas,
            light_tiles,
            restir,
            denoiser,
            denoise,
            tsr,
            upscale,
            frame,
            scene,
            scatter,
            camera,
            previous,
            feedback,
            ..
        } = self;
        let meshes: &'g Meshes = meshes;
        let pool = meshes.add_passes(g, uploads);
        let gpu = scene.add_passes(g, scatter);
        let rows = scene.instance_rows();
        let rt = tlas.add_passes(g, gpu, &pool, rows);
        let seed = *frame as u32;
        let tiles = light_tiles.add_passes(g, scene, seed);
        let scene: &'g RenderScene = scene;
        let changed_lights = scene.light_changes();
        let visibility: &'g Visibility = visibility;
        let env = *scene.environment();
        let lit = camera.map(|camera| {
            let view = ViewDesc {
                id: MAIN_VIEW,
                camera,
                previous: *previous,
                width,
                height,
            };
            let vis = visibility.add_passes(g, meshes, scene, gpu, pool, &view);
            let out = materials.add_passes(g, gpu, &vis, camera, *previous, width, height);
            let material = out.material;
            let shadow = vsm.add_passes(
                g,
                MAIN_VIEW,
                visibility,
                meshes,
                scene,
                gpu,
                vis.pages,
                vis.used,
                vis.depth,
                Some(material),
                &env,
                camera,
                width,
                height,
            );
            feedback.push_back((
                pool.frame,
                [
                    g.readback(vis.counters),
                    g.readback(vis.requests),
                    g.readback(shadow.used),
                ],
            ));
            (material, shadow.mask, vis.depth, out.velocity, camera)
        });
        let mut scene =
            lighting.add_passes(g, lit.map(|l| l.0), lit.map(|l| l.1), &env, width, height);
        // The local lights, if there are any to draw.
        if let (Some((material, _, depth, velocity, camera)), Some(tiles)) = (lit, tiles) {
            let inputs = RestirInputs {
                view: MAIN_VIEW,
                changed_lights,
                velocity,
                material,
                depth,
                lights: gpu.lights,
                rt,
                tiles,
            };
            let signal = restir.add_passes(g, inputs, camera, width, height, seed);
            let light = if *denoise {
                let inputs = DenoiseInputs {
                    view: MAIN_VIEW,
                    material,
                    depth,
                    velocity,
                };
                denoiser.add_passes(g, signal, inputs, camera, width, height, seed)
            } else {
                signal.light
            };
            scene = denoiser.add_compose(g, scene, light, material, depth, width, height);
        }
        let exposure = PostProcess::exposure(g, MAIN_VIEW);
        if let (Some(_), Some((_, _, depth, velocity, camera))) = (*upscale, lit) {
            let inputs = UpscaleInputs {
                color: scene,
                depth,
                velocity,
                exposure: exposure.handle,
                jitter: Vec2::new(camera.1.jitter[0], camera.1.jitter[1]),
                render,
                display,
            };
            scene = tsr.add_passes(g, MAIN_VIEW, &inputs);
        }
        post.add_passes(g, scene, target, exposure, dt).output
    }

    /// Takes the streaming feedback of the frames `frames` has finished (every frame's readbacks
    /// must be taken, so call this once a frame with the context [`add_passes`] was declared
    /// into) and streams pages by it.
    ///
    /// [`add_passes`]: RenderSide::add_passes
    pub fn collect_feedback(&mut self, frames: &mut FrameContext) {
        self.tlas.release(frames);
        while let Some(&(frame, [c, r, u])) = self.feedback.front() {
            let Some(counter_bytes) = frames.readback(c) else {
                break;
            };
            self.feedback.pop_front();
            // The same frame's readbacks finish together.
            let (Some(requests), Some(used)) = (frames.readback(r), frames.readback(u)) else {
                continue;
            };
            let count = counter_bytes
                .get(counters::REQUESTS * 4..)
                .and_then(|b| b.first_chunk::<4>())
                .map_or(0, |w| u32::from_le_bytes(*w));
            self.meshes.apply_feedback(&Feedback {
                frame,
                used: &used,
                requests: &requests,
                request_count: count,
            });
        }
    }

    /// Whether page streaming has caught up: the last feedback taken asked for no page that is
    /// still missing, so the frame it came from drew every page it wanted.
    pub fn streaming_idle(&self) -> bool {
        self.meshes.streaming_idle()
    }
}

/// Why a [`RenderSide`] could not be made.
#[derive(Debug, marlin_error::Error)]
pub enum RenderSideError {
    #[error("engine shaders")]
    Shaders(#[from] BindError),
    #[error("gpu")]
    Gpu(#[from] marlin_rhi::Error),
}
