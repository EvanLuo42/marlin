//! A scene drawn through the GPU Scene, the visibility passes and the material passes into a
//! small view, read back: the material buffer, the velocity, the visibility buffer, the bins'
//! counts and optionally a debug view.

use marlin_cook_render::cook_pairing;
use marlin_gpu_test_support::{Image, Shader, engine_shaders, hardware_device};
use marlin_material::{DefaultMaterial, MaterialDebug, MaterialShaders, Materials, RESOLVE};
use marlin_math::{Transform, Vec3};
use marlin_render::asset::{PairingPattern, PairingPatternView};
use marlin_render::denoise::{DenoiseInputs, DenoiseShaders, Denoiser};
use marlin_render::lights::LightTiles;
use marlin_render::raytracing::{RtShaders, Tlas};
use marlin_render::restir::{RestirDi, RestirInputs, RestirShaders};
use marlin_render::scene::{
    InstanceDesc, InstanceId, InstanceUpdate, MaterialDesc, MaterialId, MaterialUpdate, MeshId,
    MeshUpdate,
};
use marlin_render::upscale::{Tsr, TsrShaders, UpscaleInputs, UpscaleMode, Upscaler};
use marlin_render::{
    Camera, Environment, Lighting, Meshes, PagePoolDesc, RenderScene, ScatterShader, SceneUpdates,
    StreamingShaders, Visibility, VisibilityShaders,
};
use marlin_render_graph::{
    Access, FrameContext, FrameContextDesc, PassContext, TextureDesc as GraphTextureDesc,
};
use marlin_rhi::{ComputePipeline, ComputePipelineDesc, Device, Format};
use marlin_serial::cooked::Pod;

/// The render crate's test meshes, cooked by the real cooker.
#[path = "../../../render/tests/support/cook.rs"]
pub mod cook;

pub const WIDTH: u32 = 96;
pub const HEIGHT: u32 = 64;

/// The permutations the harness registers.
pub const DEFAULT: u32 = 0;
pub const STRIPES: u32 = 1;

pub struct Harness {
    pub device: &'static Device,
    pub meshes: Meshes,
    pub scene: RenderScene,
    scatter: ScatterShader,
    vis: Visibility,
    pub materials: Materials,
    lighting: Lighting,
    unorm: &'static ComputePipeline,
    signal: &'static ComputePipeline,
    /// The ray tracing scene, the light tiles and ReSTIR DI.
    pub tlas: Tlas,
    light_tiles: LightTiles,
    pub restir: RestirDi,
    /// Denoises ReSTIR's light when `denoise` is on; off, the frames are the raw signal.
    pub denoiser: Denoiser,
    pub denoise: bool,
    /// Resolves [`upscaled_frame`](Self::upscaled_frame)'s frames.
    pub tsr: Tsr,
    /// The lights [`load`](Self::load) added, in the scene's order.
    loaded_lights: Vec<marlin_render::scene::LightId>,
    pub updates: SceneUpdates,
    pub camera: (Transform, Camera),
    /// The camera of the last frame.
    previous: Option<(Transform, Camera)>,
    frame: FrameContext,
}

/// One frame's readbacks, row-major `WIDTH` × `HEIGHT`.
pub struct Frame {
    pub material: Vec<[u32; 4]>,
    pub velocity: Vec<[f32; 2]>,
    pub visbuffer: Vec<u64>,
    pub counts: Vec<u32>,
    pub debug: Option<Image>,
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes(*w))
        .collect()
}

impl Harness {
    /// The pipelines, an empty scene and a camera at (0, 0, 4) looking down -Z.
    pub fn new() -> Self {
        let device = hardware_device();
        let scatter =
            ScatterShader::new(device, engine_shaders(&[ScatterShader::ENTRY])[0]).unwrap();
        let visibility =
            VisibilityShaders::from_modules(engine_shaders(&VisibilityShaders::ENTRIES));
        let mut vis = Visibility::new(device, &visibility).unwrap();
        vis.lod_error_pixels = 0.0;
        let mut materials = Materials::new(
            device,
            &MaterialShaders::from_modules(engine_shaders(&MaterialShaders::ENTRIES)),
        )
        .unwrap();
        let resolve = |ty: &'static str| {
            let shader = Shader::new(RESOLVE.name)
                .with_type_args(&[ty])
                .in_crate(env!("CARGO_MANIFEST_DIR"));
            RESOLVE.check(shader.reflection()).unwrap();
            shader.module()
        };
        let default = materials
            .register(
                device,
                "Marlin.Engine.DefaultMaterial",
                resolve("Marlin.Engine.DefaultMaterial"),
            )
            .unwrap();
        let stripes = materials
            .register(device, "Material.Stripes", resolve("Material.Stripes"))
            .unwrap();
        assert_eq!((default, stripes), (DEFAULT, STRIPES));
        let lighting = Lighting::new(device, engine_shaders(&[Lighting::ENTRY])[0]).unwrap();
        let tlas = Tlas::new(
            device,
            &RtShaders::from_modules(engine_shaders(&RtShaders::ENTRIES)),
        )
        .unwrap();
        let light_tiles = LightTiles::new(device, engine_shaders(&[LightTiles::ENTRY])[0]).unwrap();
        // Pairing patterns cooked as the cooker would, at the radius the engine's 30 pixels at
        // 1080p come to in this small view (30 · 64 / 1080 ≈ 2), so the neighbors are as near
        // in the scene as they would be on a real screen.
        let pairing: &'static [u8] = Box::leak(
            cook_pairing(&PairingPattern {
                radius: 3.0,
                ..PairingPattern::default()
            })
            .unwrap()
            .into_boxed_slice(),
        );
        let restir = RestirDi::new(
            device,
            &RestirShaders::from_modules(engine_shaders(&RestirShaders::ENTRIES)),
            PairingPatternView::new(pairing).unwrap(),
        )
        .unwrap();
        let signal = Box::leak(Box::new(
            device
                .create_compute_pipeline(&ComputePipelineDesc {
                    shader: Shader::new("Material.WriteSignal")
                        .in_crate(env!("CARGO_MANIFEST_DIR"))
                        .module(),
                    cache: None,
                })
                .unwrap(),
        ));
        let unorm = Box::leak(Box::new(
            device
                .create_compute_pipeline(&ComputePipelineDesc {
                    shader: Shader::new("Material.ToUnorm")
                        .in_crate(env!("CARGO_MANIFEST_DIR"))
                        .module(),
                    cache: None,
                })
                .unwrap(),
        ));
        Self {
            device,
            meshes: Meshes::new(
                device,
                &PagePoolDesc {
                    slots: 64,
                    table_entries: 1024,
                    rt: marlin_render::raytracing::RtDesc {
                        geometry_bytes: 16 << 20,
                        blas_bytes: 16 << 20,
                        mesh_slots: 1024,
                        triangles_per_frame: 1 << 20,
                    },
                },
                &StreamingShaders::from_modules(engine_shaders(&StreamingShaders::ENTRIES)),
            )
            .unwrap(),
            scene: RenderScene::new(),
            scatter,
            vis,
            materials,
            lighting,
            unorm,
            signal,
            tlas,
            light_tiles,
            restir,
            denoiser: Denoiser::new(
                device,
                &DenoiseShaders::from_modules(engine_shaders(&DenoiseShaders::ENTRIES)),
            )
            .unwrap(),
            denoise: false,
            tsr: Tsr::new(
                device,
                &TsrShaders::from_modules(engine_shaders(&TsrShaders::ENTRIES)),
            )
            .unwrap(),
            loaded_lights: Vec::new(),
            updates: SceneUpdates {
                environment: Some(relative_environment()),
                ..SceneUpdates::default()
            },
            camera: (
                Transform::from_translation(Vec3::new(0.0, 0.0, 4.0)),
                Camera::default(),
            ),
            previous: None,
            frame: FrameContext::new(device, &FrameContextDesc::default()).unwrap(),
        }
    }

    /// Loads a prefab test scene: its meshes, its instances under the default material (their
    /// albedos the instance colors), its lights, environment and camera.
    pub fn load(&mut self, scene: &marlin_test_scenes::Scene) {
        let meshes: Vec<MeshId> = scene
            .meshes
            .iter()
            .map(|m| self.upload(&m.cooked()))
            .collect();
        let material = self.default_material(DefaultMaterial::default());
        for i in &scene.instances {
            let [r, g, b] = i.albedo;
            self.add(meshes[i.mesh], material, i.transform, [r, g, b, 1.0]);
        }
        for &light in &scene.lights {
            let id = self.scene.ids().lights.reserve();
            self.updates
                .lights
                .push(id, marlin_render::scene::LightUpdate::Add(light));
            self.loaded_lights.push(id);
        }
        self.updates.environment = Some(scene.environment);
        self.camera = scene.camera;
    }

    /// The id of the scene's light `i` that [`load`](Self::load) added.
    pub fn light_id(&self, i: usize) -> marlin_render::scene::LightId {
        self.loaded_lights[i]
    }

    pub fn upload(&mut self, mesh: &cook::Cooked) -> MeshId {
        let handle = self
            .meshes
            .upload_mesh(self.device, std::sync::Arc::new(mesh.clone()))
            .unwrap();
        // Every page resident: these tests are about materials, not streaming.
        assert!(self.meshes.prefetch(handle));
        let id = self.scene.ids().meshes.reserve();
        self.updates.meshes.push(id, MeshUpdate::Add);
        let resident = self.meshes.resident_mesh(handle).unwrap();
        self.updates.meshes.push(id, MeshUpdate::Resident(resident));
        id
    }

    pub fn material(&mut self, desc: MaterialDesc) -> MaterialId {
        let id = self.scene.ids().materials.reserve();
        self.updates.materials.push(id, MaterialUpdate::Add(desc));
        id
    }

    pub fn default_material(&mut self, m: DefaultMaterial) -> MaterialId {
        self.material(m.desc())
    }

    pub fn add(
        &mut self,
        mesh: MeshId,
        material: MaterialId,
        transform: Transform,
        color: [f32; 4],
    ) -> InstanceId {
        let id = self.scene.ids().instances.reserve();
        self.updates.instances.push(
            id,
            InstanceUpdate::Add(InstanceDesc {
                mesh,
                material,
                transform,
                color,
                flags: 0,
            }),
        );
        id
    }

    /// Applies the pending updates and draws one frame, reading back what the tests check.
    pub fn frame(&mut self, debug: Option<MaterialDebug>) -> Frame {
        let updates = core::mem::take(&mut self.updates);
        self.scene.apply(&updates);
        let uploads = self.meshes.take_uploads();
        let (meshes, scene, scatter, vis, materials) = (
            &self.meshes,
            &mut self.scene,
            &self.scatter,
            &self.vis,
            &self.materials,
        );
        let (camera, previous) = (self.camera, self.previous);
        let (tickets, debug_ticket) = self
            .frame
            .execute(self.device, |g, _| {
                let pool = meshes.add_passes(g, uploads);
                let gpu = scene.add_passes(g, scatter);
                let view = marlin_render::visibility::ViewDesc {
                    id: 0,
                    camera,
                    previous,
                    width: WIDTH,
                    height: HEIGHT,
                };
                let v = vis.add_passes(g, meshes, scene, gpu, pool, &view);
                let out = materials.add_passes(g, gpu, &v, camera, previous, WIDTH, HEIGHT);
                let debug = debug.map(|mode| {
                    let view = materials.add_debug_pass(g, &out, mode);
                    g.readback_texture(view)
                });
                (
                    [
                        g.readback_texture(out.material),
                        g.readback_texture(out.velocity),
                        g.readback_texture(out.visbuffer),
                        g.readback(out.counts),
                    ],
                    debug,
                )
            })
            .unwrap();
        let [material, velocity, visbuffer, counts] =
            tickets.map(|t| self.frame.wait_readback(t).unwrap());
        let debug =
            debug_ticket.map(|t| Image::new(WIDTH, HEIGHT, self.frame.wait_readback(t).unwrap()));
        assert_eq!(self.device.validation_errors(), 0);
        self.previous = Some(self.camera);
        let half = |b: [u8; 2]| marlin_render::asset::mesh::f16_to_f32(u16::from_le_bytes(b));
        Frame {
            material: words(&material).as_chunks::<4>().0.to_vec(),
            velocity: velocity
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| [half([p[0], p[1]]), half([p[2], p[3]])])
                .collect(),
            visbuffer: visbuffer
                .as_chunks::<8>()
                .0
                .iter()
                .map(|b| u64::from_le_bytes(*b))
                .collect(),
            counts: words(&counts),
            debug,
        }
    }
}

/// The proxy lobe of a pixel's dense words (`Marlin.Substrate.UnpackProxy`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Proxy {
    pub count: u32,
    pub fast: bool,
    pub normal: [f32; 3],
    pub albedo: [f32; 3],
    pub roughness: f32,
    pub f0: [f32; 3],
}

pub fn proxy(d: [u32; 4]) -> Proxy {
    let color =
        |w: u32| [w & 255, (w >> 8) & 255, (w >> 16) & 255].map(|c| (c as f32 / 255.0).powi(2));
    let e = [(d[1] & 65535) as f32, (d[1] >> 16) as f32].map(|c| c / 65535.0 * 2.0 - 1.0);
    let z = 1.0 - e[0].abs() - e[1].abs();
    let t = (-z).max(0.0);
    let n = [e[0] - t.copysign(e[0]), e[1] - t.copysign(e[1]), z];
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    Proxy {
        count: d[0] & 7,
        fast: d[0] & 8 != 0,
        normal: n.map(|c| c / l),
        albedo: color(d[2]),
        roughness: (d[2] >> 24) as f32 / 255.0,
        f0: color(d[3]),
    }
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct UnormArgs {
    scene: u32,
    output: u32,
    width: u32,
    height: u32,
}

impl Harness {
    /// Applies the pending updates and draws one `width` × `height` frame through the whole
    /// pipeline, lit, as an RGBA8 image clamped to [0, 1] (what the forward pass's targets held).
    pub fn lit_image(&mut self, width: u32, height: u32) -> Image {
        let updates = core::mem::take(&mut self.updates);
        self.scene.apply(&updates);
        let uploads = self.meshes.take_uploads();
        let (meshes, scene, scatter, vis, materials, lighting, unorm) = (
            &self.meshes,
            &mut self.scene,
            &self.scatter,
            &self.vis,
            &self.materials,
            &self.lighting,
            self.unorm,
        );
        let (camera, previous) = (self.camera, self.previous);
        let ticket = self
            .frame
            .execute(self.device, |g, _| {
                let pool = meshes.add_passes(g, uploads);
                let gpu = scene.add_passes(g, scatter);
                let view = marlin_render::visibility::ViewDesc {
                    id: 0,
                    camera,
                    previous,
                    width,
                    height,
                };
                let v = vis.add_passes(g, meshes, scene, gpu, pool, &view);
                let out = materials.add_passes(g, gpu, &v, camera, previous, width, height);
                let env = *scene.environment();
                let lit = lighting.add_passes(g, Some(out.material), None, &env, width, height);
                let target = g.create_texture(
                    "Unorm",
                    GraphTextureDesc::new_2d(width, height, Format::Rgba8Unorm),
                );
                let target = g.add_compute("ToUnorm", |b| {
                    let lit = b.read_texture(lit, Access::ComputeRead);
                    let target = b.write_texture(target, Access::ComputeWrite);
                    (target, move |ctx: &mut PassContext<'_>| {
                        ctx.set_pipeline(unorm);
                        ctx.push(&UnormArgs {
                            scene: ctx.srv(lit),
                            output: ctx.uav(target),
                            width,
                            height,
                        });
                        ctx.dispatch(width.div_ceil(8), height.div_ceil(8), 1);
                    })
                });
                g.readback_texture(target)
            })
            .unwrap();
        let pixels = self.frame.wait_readback(ticket).unwrap();
        assert_eq!(self.device.validation_errors(), 0);
        self.previous = Some(self.camera);
        Image::new(width, height, pixels)
    }
}

impl Harness {
    /// Applies the pending updates and draws one `WIDTH` × `HEIGHT` frame lit by the sun, the
    /// ambient light and, through ReSTIR DI drawn with `seed`, the scene's local lights: the HDR
    /// scene's RGB, row-major.
    pub fn restir_frame(&mut self, seed: u32) -> Vec<[f32; 3]> {
        self.restir_frame_at(seed, WIDTH, HEIGHT)
    }

    /// [`restir_frame`](Self::restir_frame) at `width` × `height`.
    pub fn restir_frame_at(&mut self, seed: u32, width: u32, height: u32) -> Vec<[f32; 3]> {
        let updates = core::mem::take(&mut self.updates);
        self.scene.apply(&updates);
        self.meshes.prepare_rt(self.device).unwrap();
        let rows = self.scene.instance_rows();
        self.tlas.prepare(self.device, rows).unwrap();
        self.tlas.release(&mut self.frame);
        let uploads = self.meshes.take_uploads();
        let (meshes, scene, scatter, vis, materials, lighting) = (
            &self.meshes,
            &mut self.scene,
            &self.scatter,
            &self.vis,
            &self.materials,
            &self.lighting,
        );
        let (tlas, tiles, restir) = (&self.tlas, &self.light_tiles, &self.restir);
        let (denoiser, denoise) = (&self.denoiser, self.denoise);
        let (camera, previous) = (self.camera, self.previous);
        let ticket = self
            .frame
            .execute(self.device, |g, _| {
                let pool = meshes.add_passes(g, uploads);
                let gpu = scene.add_passes(g, scatter);
                let rt = tlas.add_passes(g, gpu, &pool, rows);
                let drawn = tiles.add_passes(g, scene, seed);
                let view = marlin_render::visibility::ViewDesc {
                    id: 0,
                    camera,
                    previous,
                    width,
                    height,
                };
                let v = vis.add_passes(g, meshes, scene, gpu, pool, &view);
                let out = materials.add_passes(g, gpu, &v, camera, previous, width, height);
                let env = *scene.environment();
                let mut lit = lighting.add_passes(g, Some(out.material), None, &env, width, height);
                if let Some(tiles) = drawn {
                    let inputs = RestirInputs {
                        view: 0,
                        changed_lights: scene.light_changes(),
                        velocity: out.velocity,
                        material: out.material,
                        depth: v.depth,
                        lights: gpu.lights,
                        rt,
                        tiles,
                    };
                    let signal = restir.add_passes(g, inputs, camera, width, height, seed);
                    let light = if denoise {
                        let inputs = DenoiseInputs {
                            view: 0,
                            material: out.material,
                            depth: v.depth,
                            velocity: out.velocity,
                        };
                        denoiser.add_passes(g, signal, inputs, camera, width, height, seed)
                    } else {
                        signal.light
                    };
                    lit = denoiser.add_compose(g, lit, light, out.material, v.depth, width, height);
                }
                g.readback_texture(lit)
            })
            .unwrap();
        let bytes = self.frame.wait_readback(ticket).unwrap();
        assert_eq!(self.device.validation_errors(), 0);
        self.previous = Some(self.camera);
        bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|p| {
                let h = |i: usize| {
                    marlin_render::asset::mesh::f16_to_f32(u16::from_le_bytes([p[i], p[i + 1]]))
                };
                [h(0), h(2), h(4)]
            })
            .collect()
    }
}

/// The environment these tests light with: their pictures are the lit scene read without
/// exposure, so R1's relative values (a sun whose illuminance over π is 1, its ambient and
/// background) rather than physical ones.
fn relative_environment() -> Environment {
    Environment {
        sun_illuminance: core::f32::consts::PI,
        ambient: [0.08, 0.09, 0.12],
        clear_color: [0.02, 0.02, 0.03, 1.0],
        ..Environment::default()
    }
}

impl Harness {
    /// The GPU time of each pass of a recent frame, in milliseconds, by name.
    pub fn pass_times(&self) -> Vec<(&'static str, f64)> {
        self.frame
            .gpu_timings()
            .iter()
            .map(|t| (t.name, t.nanoseconds as f64 / 1e6))
            .collect()
    }
}

/// `Material.SignalArgs`.
#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct SignalArgs {
    light: u32,
    hints: u32,
    light_out: u32,
    hints_out: u32,
    width: u32,
    height: u32,
}

impl Harness {
    /// Draws one `WIDTH` × `HEIGHT` frame of the scene's surfaces (the denoiser's guides) and
    /// denoises the synthetic signal `light` (demodulated light and hit distance, row-major) with
    /// the hints `hints` (`Novelty`, `CorrelationRadius`), drawn with `seed`; returns the
    /// denoised light.
    pub fn denoise_signal(
        &mut self,
        seed: u32,
        light: &[[f32; 4]],
        hints: &[[f32; 2]],
    ) -> Vec<[f32; 3]> {
        let (width, height) = (WIDTH, HEIGHT);
        let updates = core::mem::take(&mut self.updates);
        self.scene.apply(&updates);
        let uploads = self.meshes.take_uploads();
        let (meshes, scene, scatter, vis, materials, denoiser, pipeline) = (
            &self.meshes,
            &mut self.scene,
            &self.scatter,
            &self.vis,
            &self.materials,
            &self.denoiser,
            self.signal,
        );
        let (camera, previous) = (self.camera, self.previous);
        let ticket = self
            .frame
            .execute(self.device, |g, _| {
                let pool = meshes.add_passes(g, uploads);
                let gpu = scene.add_passes(g, scatter);
                let view = marlin_render::visibility::ViewDesc {
                    id: 0,
                    camera,
                    previous,
                    width,
                    height,
                };
                let v = vis.add_passes(g, meshes, scene, gpu, pool, &view);
                let out = materials.add_passes(g, gpu, &v, camera, previous, width, height);
                let light_tex = g.create_texture(
                    "SyntheticLight",
                    GraphTextureDesc::new_2d(width, height, marlin_render::denoise::SIGNAL_FORMAT),
                );
                let hints_tex = g.create_texture(
                    "SyntheticHints",
                    GraphTextureDesc::new_2d(width, height, marlin_render::denoise::HINTS_FORMAT),
                );
                let (light_tex, hints_tex) = g.add_compute("WriteSignal", |b| {
                    let l = b.write_texture(light_tex, Access::ComputeWrite);
                    let h = b.write_texture(hints_tex, Access::ComputeWrite);
                    ((l, h), move |ctx: &mut PassContext<'_>| {
                        ctx.set_pipeline(pipeline);
                        let args = SignalArgs {
                            light: ctx.upload_slice(light),
                            hints: ctx.upload_slice(hints),
                            light_out: ctx.uav(l),
                            hints_out: ctx.uav(h),
                            width,
                            height,
                        };
                        ctx.push(&args);
                        ctx.dispatch(width.div_ceil(8), height.div_ceil(8), 1);
                    })
                });
                let signal = marlin_render::denoise::NoisySignal {
                    light: light_tex,
                    hints: hints_tex,
                };
                let inputs = DenoiseInputs {
                    view: 0,
                    material: out.material,
                    depth: v.depth,
                    velocity: out.velocity,
                };
                let denoised = denoiser.add_passes(g, signal, inputs, camera, width, height, seed);
                g.readback_texture(denoised)
            })
            .unwrap();
        let bytes = self.frame.wait_readback(ticket).unwrap();
        assert_eq!(self.device.validation_errors(), 0);
        self.previous = Some(self.camera);
        bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|p| {
                let h = |i: usize| {
                    marlin_render::asset::mesh::f16_to_f32(u16::from_le_bytes([p[i], p[i + 1]]))
                };
                [h(0), h(2), h(4)]
            })
            .collect()
    }
}

impl Harness {
    /// Frame `frame` of the view at the display size `display`, lit by the environment: drawn at
    /// `mode`'s render size with that frame's jitter and resolved by TSR (its history carried
    /// from the last call); or, with `mode` `None`, drawn at `display` with the camera's own jitter
    /// and no resolve. The HDR
    /// scene's RGB, row-major.
    pub fn upscaled_frame(
        &mut self,
        frame: u64,
        mode: Option<UpscaleMode>,
        display: (u32, u32),
    ) -> Vec<[f32; 3]> {
        let display = marlin_math::UVec2::new(display.0, display.1);
        let render = mode.map_or(display, |m| self.tsr.render_size(display, m));
        let jitter = match mode {
            Some(_) => self.tsr.jitter(frame, render, display),
            None => marlin_math::Vec2::new(self.camera.1.jitter[0], self.camera.1.jitter[1]),
        };
        self.camera.1.jitter = [jitter.x, jitter.y];
        let updates = core::mem::take(&mut self.updates);
        self.scene.apply(&updates);
        let uploads = self.meshes.take_uploads();
        let (meshes, scene, scatter, vis, materials, lighting, tsr) = (
            &self.meshes,
            &mut self.scene,
            &self.scatter,
            &self.vis,
            &self.materials,
            &self.lighting,
            &self.tsr,
        );
        let (camera, previous) = (self.camera, self.previous);
        let (width, height) = (render.x, render.y);
        let ticket = self
            .frame
            .execute(self.device, |g, _| {
                let pool = meshes.add_passes(g, uploads);
                let gpu = scene.add_passes(g, scatter);
                let view = marlin_render::visibility::ViewDesc {
                    id: 0,
                    camera,
                    previous,
                    width,
                    height,
                };
                let v = vis.add_passes(g, meshes, scene, gpu, pool, &view);
                let out = materials.add_passes(g, gpu, &v, camera, previous, width, height);
                let env = *scene.environment();
                let mut lit = lighting.add_passes(g, Some(out.material), None, &env, width, height);
                if mode.is_some() {
                    // No post chain: an exposure of 0, which TSR takes as 1.
                    let exposure = marlin_render::PostProcess::exposure(g, 0);
                    let mut e = exposure.handle;
                    if exposure.fresh {
                        e = g.add_compute("ExposureClear", |b| {
                            let e = b.write_buffer(e, Access::Clear);
                            (e, move |ctx: &mut PassContext<'_>| ctx.fill_buffer(e, 0))
                        });
                    }
                    let inputs = UpscaleInputs {
                        color: lit,
                        depth: v.depth,
                        velocity: out.velocity,
                        exposure: e,
                        jitter,
                        render,
                        display,
                    };
                    lit = tsr.add_passes(g, 0, &inputs);
                }
                g.readback_texture(lit)
            })
            .unwrap();
        let bytes = self.frame.wait_readback(ticket).unwrap();
        assert_eq!(self.device.validation_errors(), 0);
        self.previous = Some(self.camera);
        bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|p| {
                let h = |i: usize| {
                    marlin_render::asset::mesh::f16_to_f32(u16::from_le_bytes([p[i], p[i + 1]]))
                };
                [h(0), h(2), h(4)]
            })
            .collect()
    }
}
