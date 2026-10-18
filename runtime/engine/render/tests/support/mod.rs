//! The forward renderer on the shared hardware device, drawing through a frame context into a
//! small transient target that is read back after each frame. Used by the `gpu` and `golden`
//! binaries.

use marlin_ecs::World;
use marlin_gpu_test_support::{Image, hardware_device, shader};
use marlin_math::{Transform, Vec3};
use marlin_render::{
    Camera, Environment, ForwardShaders, MeshData, MeshHandle, RenderStats, Renderer, RendererDesc,
    Scene, cube,
};
use marlin_render_graph::{FrameContext, FrameContextDesc, TextureDesc};
use marlin_rhi::{ClearValue, Device, Format};

pub const SIZE: u32 = 64;

pub struct Harness {
    pub device: &'static Device,
    pub renderer: Renderer,
    pub cube: MeshHandle,
    frame: FrameContext,
}

impl Harness {
    /// A renderer with `environment` and the unit cube uploaded.
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
        renderer.environment = environment;
        let (vertices, indices) = cube();
        let cube = renderer
            .upload_mesh(
                device,
                &MeshData {
                    vertices: &vertices,
                    indices: &indices,
                },
            )
            .unwrap();
        let frame = FrameContext::new(device, &FrameContextDesc::default()).unwrap();
        Self {
            device,
            renderer,
            cube,
            frame,
        }
    }

    /// Renders `world` and reads the frame back.
    pub fn render_image(&mut self, world: &World) -> (Image, RenderStats) {
        let scene = Scene::extract(world);
        let renderer = &self.renderer;
        let (ticket, stats) = self
            .frame
            .execute(self.device, |g, _| {
                let target = g.create_texture(
                    "Target",
                    TextureDesc::new_2d(SIZE, SIZE, Format::Rgba8Unorm)
                        .with_clear(ClearValue::Color(renderer.environment.clear_color)),
                );
                let (target, stats) = renderer.add_passes(g, target, &scene);
                (g.readback_texture(target), stats)
            })
            .unwrap();
        let pixels = self.frame.wait_readback(ticket).unwrap();
        assert_eq!(self.device.validation_errors(), 0);
        (Image::new(SIZE, SIZE, pixels), stats)
    }

    /// As [`render_image`](Self::render_image), the pixels as little-endian `u32`s (R lowest).
    #[allow(dead_code, reason = "the golden binary compares whole images")]
    pub fn render(&mut self, world: &World) -> (Vec<u32>, RenderStats) {
        let (image, stats) = self.render_image(world);
        let pixels = image
            .rgba8
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&p| u32::from_le_bytes(p))
            .collect();
        (pixels, stats)
    }

    /// Waits for every frame, so meshes can be released.
    #[allow(dead_code, reason = "only some binaries release meshes")]
    pub fn wait_idle(&mut self) {
        self.frame.wait_idle().unwrap();
    }
}

/// A camera at (0, 0, `z`) looking down -Z.
pub fn camera(world: &mut World, z: f32) {
    world.spawn((
        Transform::from_translation(Vec3::new(0.0, 0.0, z)),
        Camera::default(),
    ));
}
