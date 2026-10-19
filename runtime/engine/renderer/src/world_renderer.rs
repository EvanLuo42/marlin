//! [`WorldRenderer`]: one world's render side (0008 §5, 0019 §7). It owns the passes, the
//! [`RenderScene`] and the GPU meshes; each frame it takes the world's [`RenderDelta`], uploads,
//! applies the updates and draws a view. The host owns it, apart from the world: the world
//! simulates frame N+1 on one job while its renderer draws frame N on another.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_containers::{HashMap, HashMapExt};
use marlin_material::{
    BAKE, DEFAULT_PERMUTATION, MaterialShaders, Materials, RESOLVE, TranslucentInputs,
};
use marlin_math::{Transform, UVec2, Vec2};
use marlin_render::Environment;
use marlin_render::asset::PairingPatternView;
use marlin_render::denoise::{DenoiseInputs, DenoiseShaders, Denoiser, NoisySignal};
use marlin_render::light_grid::LightGrid;
use marlin_render::lighting::LitView;
use marlin_render::lights::LightTiles;
use marlin_render::raytracing::{RtShaders, Tlas};
use marlin_render::restir::{RestirDi, RestirInputs, RestirShaders};
use marlin_render::scene::{MeshId, MeshUpdate};
use marlin_render::shadows::{Vsm, VsmSettings, VsmShaders};
use marlin_render::sharc::{Sharc, SharcInputs, SharcShaders};
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

/// A view's specular signal's denoising history is keyed by the view's id with this bit.
const SPECULAR_BIT: u64 = 1 << 63;

use crate::assets::{RenderAssets, ShaderError};
use crate::components::ViewTarget;
use crate::delta::{RenderDelta, ViewId};

/// The render scene, the GPU copies of its meshes and the passes that draw it.
pub struct WorldRenderer {
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
    /// The clustered light grid: the local lights of translucent surfaces (0010 §5.4).
    pub light_grid: LightGrid,
    /// ReSTIR (0010 §2): local lights and GI.
    pub restir: RestirDi,
    /// Denoises ReSTIR's light (0010 §6); `denoise` off composes it as it is.
    pub denoiser: Denoiser,
    pub denoise: bool,
    /// Resolves the view, drawn at `upscale`'s render size with a jitter a frame, to the target's
    /// size (0010 §7): TSR, at the native size by default, so it is the anti-aliasing. `None`
    /// draws at the target's size without jitter or resolve.
    pub tsr: Tsr,
    pub upscale: Option<UpscaleMode>,
    /// The world radiance cache (0010 §4.1), where the GPU has 64-bit atomics: what GI reads.
    /// Without it (or with `restir.gi` off) there is no GI, the ambient light standing in.
    /// `sharc_view` (`r.sharc.view`) draws the radiance it holds in place of the lit scene.
    pub sharc: Option<Sharc>,
    pub sharc_view: bool,
    /// Tonemaps the [`HDR_FORMAT`](marlin_render::HDR_FORMAT) scene into the output.
    pub post: PostProcess,
    pub scene: RenderScene,
    scatter: ScatterShader,
    /// The last applied delta's time step and views; each view's camera as last drawn (with its
    /// jitter), and the one before it.
    dt: f32,
    views: Vec<crate::delta::RenderView>,
    cameras: HashMap<ViewId, (Transform, Camera)>,
    previous: HashMap<ViewId, (Transform, Camera)>,
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

impl WorldRenderer {
    /// A renderer for the world whose scene rows come from `ids` ([`crate::scene_ids`]), made with
    /// the render module's engine assets (loaded in `assets`); its post chain writes `output`.
    pub fn new(
        device: &Device,
        assets: &marlin_asset::AssetServer,
        engine: &RenderAssets,
        output: Format,
        ids: SceneIds,
    ) -> Result<Self, WorldRendererError> {
        let missing =
            || WorldRendererError::Assets(ShaderError::Load(marlin_asset::AssetError::NotFound));
        Self::with_shaders(
            device,
            assets.get(&engine.library).ok_or_else(missing)?,
            assets.get(&engine.resolve).ok_or_else(missing)?,
            assets.get(&engine.bake).ok_or_else(missing)?,
            assets.get(&engine.pairing).ok_or_else(missing)?.view(),
            output,
            ids,
        )
    }

    /// The passes made with the engine's shaders (`engine`, the `Marlin.Engine` library), the
    /// default material's resolve permutation (`default_resolve`, `DEFAULT_RESOLVE`) and its
    /// simplified-material bake (`default_bake`, `DEFAULT_BAKE`) and ReSTIR's
    /// pairing patterns (`pairing`, `PAIRING_PATTERN`); the post chain writes `output`. The
    /// scene's rows come from `ids`, the allocators the simulation reserves them from.
    pub fn with_shaders(
        device: &Device,
        engine: &ShaderLibrary,
        default_resolve: &ShaderLibrary,
        default_bake: &ShaderLibrary,
        pairing: PairingPatternView<'_>,
        output: Format,
        ids: SceneIds,
    ) -> Result<Self, WorldRendererError> {
        let mut materials = Materials::new(device, &MaterialShaders::bind(engine)?)?;
        let default = materials.register(
            device,
            "Marlin.Engine.DefaultMaterial",
            bind_one(default_resolve, &RESOLVE)?,
            bind_one(default_bake, &BAKE)?,
        )?;
        debug_assert_eq!(default, DEFAULT_PERMUTATION);
        let translucent = materials.translucent();
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
            light_grid: LightGrid::new(device, bind_one(engine, &LightGrid::ENTRY)?)?,
            restir: RestirDi::new(device, &RestirShaders::bind(engine)?, pairing)?,
            denoiser: Denoiser::new(device, &DenoiseShaders::bind(engine)?)?,
            denoise: true,
            tsr: Tsr::new(device, &TsrShaders::bind(engine)?)?,
            upscale: Some(UpscaleMode::Native),
            sharc: match device.features().atomic_int64 {
                true => Some(Sharc::new(device, &SharcShaders::bind(engine)?)?),
                false => None,
            },
            sharc_view: false,
            post: PostProcess::new(device, &PostShaders::bind(engine)?, output)?,
            scene: {
                let mut scene = RenderScene::with_ids(ids);
                scene.set_translucent(translucent);
                scene
            },
            scatter: ScatterShader::new(device, bind_one(engine, &ScatterShader::ENTRY)?)?,
            dt: 0.0,
            views: Vec::new(),
            cameras: HashMap::new(),
            previous: HashMap::new(),
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
        // Each view's last camera (as drawn, jittered) becomes its previous one; views gone from
        // the frame forget theirs.
        let mut cameras = HashMap::new();
        let mut previous = HashMap::new();
        for v in &delta.views {
            if let Some(&last) = self.cameras.get(&v.id) {
                previous.insert(v.id, last);
            }
            cameras.insert(v.id, (v.transform, v.camera));
        }
        self.cameras = cameras;
        self.previous = previous;
        self.views.clone_from(&delta.views);
        Ok(())
    }

    /// Declares the frame of the main view ([`ViewTarget::Main`]) into `g`; see
    /// [`add_view_passes`](Self::add_view_passes).
    pub fn add_passes<'g>(
        &'g mut self,
        g: &mut RenderGraph<'g>,
        target: TextureHandle<'g>,
    ) -> TextureHandle<'g> {
        let view = self.view_for(ViewTarget::Main);
        self.add_view_passes(g, view, target)
    }

    /// The view the last delta draws into `target`: its highest-priority one.
    pub fn view_for(&self, target: ViewTarget) -> Option<ViewId> {
        self.views.iter().find(|v| v.target == target).map(|v| v.id)
    }

    /// Declares the last applied delta's frame, as `view` sees it, into `g` (0008 §3): the GPU
    /// Scene's scatter, the visibility buffer and the material buffer of the view at the
    /// upscaler's render size, jittered; direct lighting and the denoised local lights into an
    /// [`HDR_FORMAT`](marlin_render::HDR_FORMAT) scene; TSR to the size of `target`; then the post
    /// chain into `target`. Histories (exposure, TSR, denoising, reservoirs) are the view's own,
    /// keyed by its id. Without a view the scene is the clear color. Needs a graph with a state
    /// table.
    pub fn add_view_passes<'g>(
        &'g mut self,
        g: &mut RenderGraph<'g>,
        view: Option<ViewId>,
        target: TextureHandle<'g>,
    ) -> TextureHandle<'g> {
        let size = *g.texture_desc(target);
        let display = UVec2::new(size.width, size.height);
        let view = view.filter(|v| self.cameras.contains_key(v));
        // The render size and this frame's jitter, which goes into the view's camera (and so into
        // its previous camera next frame).
        let render = match (self.upscale, view) {
            (Some(mode), Some(_)) => self.tsr.render_size(display, mode),
            _ => display,
        };
        if let (Some(_), Some(v)) = (self.upscale, view) {
            let jitter = self.tsr.jitter(self.frame, render, display);
            if let Some((_, camera)) = self.cameras.get_mut(&v) {
                camera.jitter = [jitter.x, jitter.y];
            }
        }
        let main_view = view.map_or(0, |v| v.0);
        let specular_view = main_view | SPECULAR_BIT;
        let camera = view.and_then(|v| self.cameras.get(&v).copied());
        let previous = view.and_then(|v| self.previous.get(&v).copied());
        let previous = &previous;
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
            light_grid,
            restir,
            denoiser,
            denoise,
            tsr,
            upscale,
            sharc,
            sharc_view,
            frame,
            scene,
            scatter,
            feedback,
            ..
        } = self;
        let meshes: &'g Meshes = meshes;
        let pool = meshes.add_passes(g, uploads);
        let gpu = scene.add_passes(g, scatter);
        let simple = materials.add_simple_passes(g, scene, gpu);
        let rows = scene.instance_rows();
        let rt = tlas.add_passes(g, gpu, &pool, rows);
        let seed = *frame as u32;
        let tiles = light_tiles.add_passes(g, scene, seed);
        let scene: &'g RenderScene = scene;
        // `scene` becomes the HDR picture below.
        let world = scene;
        let changed_lights = scene.light_changes();
        let visibility: &'g Visibility = visibility;
        let env = *scene.environment();
        // The opaque visibility, which the translucent view culls against.
        let mut opaque = None;
        let lit = camera.map(|camera| {
            let view = ViewDesc {
                id: main_view,
                camera,
                previous: *previous,
                width,
                height,
            };
            let vis = visibility.add_passes(g, meshes, scene, gpu, pool, &view);
            opaque = Some((vis, view));
            let out = materials.add_passes(g, gpu, &vis, camera, *previous, width, height);
            let material = out.material;
            let shadow = vsm.add_passes(
                g,
                main_view,
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
        // The world cache, when the reservoirs read it (GI) or its view is on.
        let sharc: &'g Option<Sharc> = sharc;
        let cache = match (sharc.as_ref(), lit) {
            (Some(sharc), Some((material, _, depth, _, camera))) if restir.gi || *sharc_view => {
                let inputs = SharcInputs {
                    material,
                    depth,
                    rt,
                    simple,
                    lights: gpu.lights,
                    tiles,
                };
                Some(sharc.add_passes(g, inputs, &env, camera, width, height, seed))
            }
            _ => None,
        };
        // With GI the reservoirs own the sky's light (0010 §2.9): the lighting pass has no
        // ambient term.
        let gi = restir.gi && cache.is_some();
        let lit_env = Environment {
            ambient: if gi { [0.0; 3] } else { env.ambient },
            ..env
        };
        let mut scene = lighting.add_passes(
            g,
            lit.map(|(material, _, depth, _, camera)| LitView {
                material,
                depth,
                camera,
            }),
            lit.map(|l| l.1),
            &lit_env,
            width,
            height,
        );
        // The reservoirs: local lights if there are any, and GI.
        if let Some((material, _, depth, velocity, camera)) = lit
            && (tiles.is_some() || gi)
        {
            let inputs = RestirInputs {
                view: main_view,
                changed_lights,
                velocity,
                material,
                depth,
                lights: gpu.lights,
                rt,
                tiles,
                simple,
                cache,
                environment: env,
            };
            let signals = restir.add_passes(g, inputs, camera, width, height, seed);
            // Each signal denoised with a history of its own: the specular one's is another view's.
            let mut denoised = |signal: NoisySignal<'g>, view| {
                if !*denoise {
                    return signal.light;
                }
                let inputs = DenoiseInputs {
                    view,
                    material,
                    depth,
                    velocity,
                    specular: view == specular_view,
                    previous: *previous,
                };
                denoiser.add_passes(g, signal, inputs, camera, width, height, seed)
            };
            let diffuse = denoised(signals.diffuse, main_view);
            let specular = denoised(signals.specular, specular_view);
            scene = denoiser.add_compose(
                g,
                scene,
                diffuse,
                Some(specular),
                material,
                depth,
                camera,
                width,
                height,
            );
        }
        let mut reactive = None;
        // The translucent surfaces over it (0009 §8), if there are any.
        if let (Some((vis, view)), Some((_, _, depth, _, camera))) = (opaque, lit)
            && !world.translucent_instances().is_empty()
        {
            let clusters = visibility.add_translucent_view(g, meshes, world, gpu, &vis, &view);
            let light_grid: &'g LightGrid = light_grid;
            let grid = (world.light_rows() > 0)
                .then(|| light_grid.add_passes(g, world, gpu.lights, camera, width, height));
            let inputs = TranslucentInputs {
                clusters,
                depth,
                rt,
                cache,
                grid,
                environment: env,
            };
            let out = materials
                .add_translucent_passes(g, world, gpu, scene, inputs, camera, width, height);
            scene = out.color;
            reactive = out.reactive;
        }
        // The radiance cache's view, in place of the lit scene.
        if let (Some(sharc), Some(cache), true, Some((material, _, depth, _, _))) =
            (sharc.as_ref(), cache, *sharc_view, lit)
        {
            scene = sharc.add_view(g, &cache, material, depth);
        }
        let exposure = PostProcess::exposure(g, main_view);
        if let (Some(_), Some((_, _, depth, velocity, camera))) = (*upscale, lit) {
            let inputs = UpscaleInputs {
                color: scene,
                depth,
                velocity,
                reactive,
                exposure: exposure.handle,
                jitter: Vec2::new(camera.1.jitter[0], camera.1.jitter[1]),
                render,
                display,
            };
            scene = tsr.add_passes(g, main_view, &inputs);
        }
        post.add_passes(g, scene, target, exposure, dt).output
    }

    /// Takes the streaming feedback of the frames `frames` has finished (every frame's readbacks
    /// must be taken, so call this once a frame with the context [`add_passes`] was declared
    /// into) and streams pages by it.
    ///
    /// [`add_passes`]: WorldRenderer::add_passes
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

/// Why a [`WorldRenderer`] could not be made.
#[derive(Debug, marlin_error::Error)]
pub enum WorldRendererError {
    #[error("the render module's assets")]
    Assets(#[from] ShaderError),
    #[error("engine shaders")]
    Shaders(#[from] BindError),
    #[error("gpu")]
    Gpu(#[from] marlin_rhi::Error),
}
