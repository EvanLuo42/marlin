//! The forward renderer on the shared hardware device, drawing a [`RenderScene`] through a frame
//! context into a small transient target that is read back after each frame. Tests change the
//! scene the way extract does, with updates on reserved handles. Used by the `gpu` and `golden`
//! binaries. Meshes are cooked by the real cooker ([`cook`]).

pub mod cook;

use marlin_gpu_test_support::{Image, hardware_device, shader};
use marlin_math::{Transform, Vec3};
use marlin_render::scene::{InstanceDesc, InstanceId, InstanceUpdate, MeshId, MeshUpdate};
use marlin_render::{
    Camera, Environment, ForwardShaders, GpuScene, MeshHandle, RenderScene, RenderStats, Renderer,
    RendererDesc, ScatterShader, SceneUpdates, Visibility,
};
use marlin_render_graph::{FrameContext, FrameContextDesc, Readback, TextureDesc};
use marlin_rhi::{ClearValue, Device, Format};

use cook::Cooked;

pub const SIZE: u32 = 64;

pub struct Harness {
    pub device: &'static Device,
    pub renderer: Renderer,
    pub scene: RenderScene,
    scatter: ScatterShader,
    /// The unit cube, in the scene and on the GPU.
    pub cube: MeshId,
    /// Applied with the next frame.
    pub updates: SceneUpdates,
    pub camera: Option<(Transform, Camera)>,
    frame: FrameContext,
}

impl Harness {
    /// A renderer, a scene lit by `environment` and the unit cube uploaded into it.
    pub fn new(environment: Environment) -> Self {
        let device = hardware_device();
        let mut renderer = Renderer::new(
            device,
            &RendererDesc {
                color_format: Format::Rgba8Unorm,
                shaders: Some(ForwardShaders {
                    vertex: shader("Marlin.Engine.ForwardVs"),
                    pixel: shader("Marlin.Engine.ForwardPs"),
                }),
            },
        )
        .unwrap();
        let scatter = ScatterShader::new(device, shader("Marlin.Engine.Scene.Scatter")).unwrap();
        let frame = FrameContext::new(device, &FrameContextDesc::default()).unwrap();
        let scene = RenderScene::new();
        let mut updates = SceneUpdates {
            environment: Some(environment),
            ..SceneUpdates::default()
        };
        let (cube, _) = upload(device, &mut renderer, &scene, &mut updates, &cook::cube());
        Self {
            device,
            renderer,
            scene,
            scatter,
            cube,
            updates,
            camera: None,
            frame,
        }
    }

    /// Uploads `mesh` and adds it to the scene, resident.
    pub fn upload(&mut self, mesh: &Cooked) -> (MeshId, MeshHandle) {
        upload(
            self.device,
            &mut self.renderer,
            &self.scene,
            &mut self.updates,
            mesh,
        )
    }

    /// Adds an instance of `mesh`.
    pub fn add(&mut self, mesh: MeshId, transform: Transform, color: [f32; 4]) -> InstanceId {
        let id = self.scene.ids().instances.reserve();
        self.updates.instances.push(
            id,
            InstanceUpdate::Add(InstanceDesc {
                mesh,
                transform,
                color,
                flags: 0,
            }),
        );
        id
    }

    #[allow(dead_code, reason = "the golden binary only adds")]
    pub fn set(&mut self, id: InstanceId, update: InstanceUpdate) {
        self.updates.instances.push(id, update);
    }

    /// A camera at (0, 0, `z`) looking down -Z.
    pub fn camera_at(&mut self, z: f32) {
        self.camera = Some((
            Transform::from_translation(Vec3::new(0.0, 0.0, z)),
            Camera::default(),
        ));
    }

    /// Applies the pending updates, renders a frame and reads it back, and with it whatever
    /// `read` asks for of the GPU Scene.
    pub fn frame_with<R>(
        &mut self,
        read: impl for<'g> FnOnce(&mut marlin_render_graph::RenderGraph<'g>, GpuScene<'g>) -> R,
    ) -> (Image, RenderStats, R) {
        let updates = core::mem::take(&mut self.updates);
        self.scene.apply(&updates);
        let (renderer, scene, scatter, camera) =
            (&self.renderer, &mut self.scene, &self.scatter, self.camera);
        let ((ticket, stats), extra) = self
            .frame
            .execute(self.device, |g, _| {
                let gpu = scene.add_passes(g, scatter);
                let clear = scene.environment().clear_color;
                let target = g.create_texture(
                    "Target",
                    TextureDesc::new_2d(SIZE, SIZE, Format::Rgba8Unorm)
                        .with_clear(ClearValue::Color(clear)),
                );
                let forward = renderer.add_passes(g, target, scene, gpu, camera);
                let (target, stats) = (forward.color, forward.stats);
                let ticket: Readback = g.readback_texture(target);
                let extra = read(g, gpu);
                ((ticket, stats), extra)
            })
            .unwrap();
        let pixels = self.frame.wait_readback(ticket).unwrap();
        assert_eq!(self.device.validation_errors(), 0);
        (Image::new(SIZE, SIZE, pixels), stats, extra)
    }

    /// Applies the pending updates and renders one frame with both the forward pass and the
    /// visibility passes of `vis`, reading back what the visibility tests compare.
    #[allow(dead_code, reason = "only the gpu binary runs the visibility passes")]
    pub fn visibility_frame(&mut self, vis: &'static Visibility) -> VisFrame {
        let updates = core::mem::take(&mut self.updates);
        self.scene.apply(&updates);
        let (renderer, scene, scatter) = (&self.renderer, &mut self.scene, &self.scatter);
        let camera = self.camera.expect("a camera");
        let (tickets, capacity) = self
            .frame
            .execute(self.device, |g, _| {
                let gpu = scene.add_passes(g, scatter);
                let clear = scene.environment().clear_color;
                let target = g.create_texture(
                    "Target",
                    TextureDesc::new_2d(SIZE, SIZE, Format::Rgba8Unorm)
                        .with_clear(ClearValue::Color(clear)),
                );
                let forward = renderer.add_passes(g, target, scene, gpu, Some(camera));
                let out = vis.add_passes(g, renderer, scene, gpu, camera, SIZE, SIZE);
                (
                    [
                        g.readback_texture(forward.color),
                        g.readback_texture(forward.depth),
                        g.readback_texture(out.visbuffer),
                        g.readback_texture(out.depth),
                        g.readback(out.counters),
                        g.readback(out.clusters),
                    ],
                    out.capacity,
                )
            })
            .unwrap();
        let [color, depth, visbuffer, vis_depth, counters, clusters] =
            tickets.map(|t| self.frame.wait_readback(t).unwrap());
        assert_eq!(self.device.validation_errors(), 0);
        let floats = |b: &[u8]| words(b).into_iter().map(f32::from_bits).collect::<Vec<_>>();
        let clusters = words(&clusters);
        VisFrame {
            color: words(&color),
            depth: floats(&depth),
            visbuffer: visbuffer
                .as_chunks::<8>()
                .0
                .iter()
                .map(|b| u64::from_le_bytes(*b))
                .collect(),
            vis_depth: floats(&vis_depth),
            counters: words(&counters)[..3].try_into().unwrap(),
            clusters: clusters.as_chunks::<2>().0.to_vec(),
            capacity,
        }
    }

    /// Renders a frame and reads it back.
    pub fn render_image(&mut self) -> (Image, RenderStats) {
        let (image, stats, ()) = self.frame_with(|_, _| ());
        (image, stats)
    }

    /// As [`render_image`](Self::render_image), the pixels as little-endian `u32`s (R lowest).
    #[allow(dead_code, reason = "the golden binary compares whole images")]
    pub fn render(&mut self) -> (Vec<u32>, RenderStats) {
        let (image, stats) = self.render_image();
        (words(&image.rgba8), stats)
    }

    /// Waits for the readback `ticket` of the last frame.
    #[allow(dead_code, reason = "only the gpu binary reads the GPU Scene")]
    pub fn wait_readback(&mut self, ticket: Readback) -> Vec<u8> {
        self.frame.wait_readback(ticket).unwrap()
    }

    /// Waits for every frame, so meshes can be released.
    #[allow(dead_code, reason = "only some binaries release meshes")]
    pub fn wait_idle(&mut self) {
        self.frame.wait_idle().unwrap();
    }
}

/// Little-endian 32-bit words of `bytes`.
pub fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&p| u32::from_le_bytes(p))
        .collect()
}

fn upload(
    device: &Device,
    renderer: &mut Renderer,
    scene: &RenderScene,
    updates: &mut SceneUpdates,
    mesh: &Cooked,
) -> (MeshId, MeshHandle) {
    let handle = renderer
        .upload_mesh(device, &mesh.view(), &mesh.pages())
        .unwrap();
    let id = scene.ids().meshes.reserve();
    updates.meshes.push(id, MeshUpdate::Add);
    let resident = renderer.resident_mesh(handle).unwrap();
    updates.meshes.push(id, MeshUpdate::Resident(resident));
    (id, handle)
}

/// One frame of [`Harness::visibility_frame`]: every image row-major, `SIZE` × `SIZE`.
#[allow(dead_code, reason = "only the gpu binary runs the visibility passes")]
pub struct VisFrame {
    /// The forward pass's color (RGBA8, R lowest) and depth.
    pub color: Vec<u32>,
    pub depth: Vec<f32>,
    pub visbuffer: Vec<u64>,
    pub vis_depth: Vec<f32>,
    /// Visible instances, visible clusters found, clusters drawn.
    pub counters: [u32; 3],
    /// Instance row, meshlet | LOD << 24, for each of the buffer's entries.
    pub clusters: Vec<[u32; 2]>,
    pub capacity: u32,
}
