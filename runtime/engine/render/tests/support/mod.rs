//! The forward renderer on the shared hardware device, drawing into a small offscreen target that
//! is read back after each frame. Used by the `gpu` and `golden` binaries.

use core::cell::Cell;

use marlin_ecs::World;
use marlin_gpu_test_support::{Image, hardware_device, read_texture, run, shader};
use marlin_math::{Transform, Vec3};
use marlin_render::{
    Camera, Environment, ForwardShaders, MeshData, MeshHandle, RenderStats, Renderer, RendererDesc,
    cube,
};
use marlin_rhi::{
    ClearValue, Device, Format, RenderTargetViewDesc, Texture, TextureBarrier, TextureDesc,
    TextureState, TextureUsage,
};

pub const SIZE: u32 = 64;

pub struct Harness {
    pub device: &'static Device,
    pub renderer: Renderer,
    pub cube: MeshHandle,
    target: Texture,
    /// The state the last frame left the target in.
    state: Cell<TextureState>,
}

impl Harness {
    /// A renderer with `environment` and the unit cube uploaded.
    pub fn new(environment: Environment) -> Self {
        let device = hardware_device();
        let mut renderer = Renderer::new(
            device,
            &RendererDesc {
                color_format: Format::Rgba8Unorm,
                frames_in_flight: 2,
                width: SIZE,
                height: SIZE,
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
        let target = device
            .create_texture(&TextureDesc {
                clear_value: Some(ClearValue::Color(environment.clear_color)),
                ..TextureDesc::new_2d(
                    SIZE,
                    SIZE,
                    Format::Rgba8Unorm,
                    TextureUsage::RENDER_TARGET | TextureUsage::COPY_SRC,
                )
            })
            .unwrap();
        Self {
            device,
            renderer,
            cube,
            target,
            state: Cell::new(TextureState::UNDEFINED),
        }
    }

    /// Renders `world` and reads the frame back.
    pub fn render_image(&mut self, world: &World) -> (Image, RenderStats) {
        let device = self.device;
        let view = device
            .create_render_target_view(&self.target, &RenderTargetViewDesc::default())
            .unwrap();
        let mut stats = None;
        run(device, |list| {
            list.texture_barriers(&[TextureBarrier::new(
                &self.target,
                self.state.get(),
                TextureState::RENDER_TARGET,
            )]);
            // SAFETY: slot 0's previous work (if any) was waited for at the end of the last call.
            stats = Some(unsafe { self.renderer.render(device, list, 0, &view, world).unwrap() });
            list.texture_barriers(&[TextureBarrier::new(
                &self.target,
                TextureState::RENDER_TARGET,
                TextureState::COPY_SRC,
            )]);
        });
        self.state.set(TextureState::COPY_SRC);
        let image = read_texture(device, &self.target);
        assert_eq!(device.validation_errors(), 0);
        (image, stats.unwrap())
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
}

/// A camera at (0, 0, `z`) looking down -Z.
pub fn camera(world: &mut World, z: f32) {
    world.spawn((
        Transform::from_translation(Vec3::new(0.0, 0.0, z)),
        Camera::default(),
    ));
}
