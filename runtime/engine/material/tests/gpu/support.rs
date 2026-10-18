//! A scene drawn through the GPU Scene, the visibility passes and the material passes into a
//! small view, read back: the material buffer, the velocity, the visibility buffer, the bins'
//! counts and optionally a debug view.

use marlin_gpu_test_support::{Image, Shader, hardware_device, shader};
use marlin_material::{DefaultMaterial, MaterialDebug, MaterialShaders, Materials};
use marlin_math::{Transform, Vec3};
use marlin_render::scene::{
    InstanceDesc, InstanceId, InstanceUpdate, MaterialDesc, MaterialId, MaterialUpdate, MeshId,
    MeshUpdate,
};
use marlin_render::{
    Camera, Environment, RenderScene, Renderer, RendererDesc, ScatterShader, SceneUpdates,
    Visibility, VisibilityShaders,
};
use marlin_render_graph::{FrameContext, FrameContextDesc};
use marlin_rhi::{Device, Format};

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
    pub renderer: Renderer,
    pub scene: RenderScene,
    scatter: ScatterShader,
    vis: Visibility,
    pub materials: Materials,
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
        let renderer = Renderer::new(
            device,
            &RendererDesc {
                color_format: Format::Rgba8Unorm,
                shaders: None,
            },
        )
        .unwrap();
        let scatter = ScatterShader::new(device, shader("Marlin.Engine.Scene.Scatter")).unwrap();
        let v = |name: &str| shader(&format!("Marlin.Engine.Visibility.{name}"));
        let mut vis = Visibility::new(
            device,
            &VisibilityShaders {
                reset: v("Reset"),
                instance_cull: v("InstanceCull"),
                instance_args: v("InstanceArgs"),
                cluster_cull: v("ClusterCull"),
                finish_args: v("FinishArgs"),
                mesh: v("VisMesh"),
                pixel: v("VisPs"),
                clear: v("VisClear"),
                depth: v("VisDepth"),
            },
        )
        .unwrap();
        vis.lod_error_pixels = 0.0;
        let m = |name: &str| shader(&format!("Marlin.Engine.Material.{name}"));
        let mut materials = Materials::new(
            device,
            &MaterialShaders {
                classify: m("Classify"),
                bins: m("Bins"),
                scatter: m("Scatter"),
                debug: m("MaterialDebug"),
            },
        )
        .unwrap();
        let resolve = |ty: &'static str| {
            Shader::new("Marlin.Engine.Material.ResolvePixel")
                .with_type_args(&[ty])
                .in_crate(env!("CARGO_MANIFEST_DIR"))
                .module()
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
        Self {
            device,
            renderer,
            scene: RenderScene::new(),
            scatter,
            vis,
            materials,
            updates: SceneUpdates {
                environment: Some(Environment::default()),
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

    pub fn upload(&mut self, mesh: &cook::Cooked) -> MeshId {
        let handle = self
            .renderer
            .upload_mesh(self.device, &mesh.view(), &mesh.pages())
            .unwrap();
        let id = self.scene.ids().meshes.reserve();
        self.updates.meshes.push(id, MeshUpdate::Add);
        let resident = self.renderer.resident_mesh(handle).unwrap();
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
        let (renderer, scene, scatter, vis, materials) = (
            &self.renderer,
            &mut self.scene,
            &self.scatter,
            &self.vis,
            &self.materials,
        );
        let (camera, previous) = (self.camera, self.previous);
        let (tickets, debug_ticket) = self
            .frame
            .execute(self.device, |g, _| {
                let gpu = scene.add_passes(g, scatter);
                let v = vis.add_passes(g, renderer, scene, gpu, camera, WIDTH, HEIGHT);
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
