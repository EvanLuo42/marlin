//! The forward renderer on the hardware adapter, with validation on: scenes rendered offscreen
//! and read back.
#![cfg(marlin_platform = "windows")]

use marlin_ecs::World;
use marlin_gpu_test_support::{hardware_device, read_words, readback, run};
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::{
    Camera, Environment, MeshData, MeshHandle, MeshRenderer, RenderStats, Renderer, RendererDesc,
    cube,
};
use marlin_rhi::{
    Buffer, BufferTextureCopy, ClearValue, Device, Format, RenderTargetViewDesc, Texture,
    TextureBarrier, TextureDesc, TextureState, TextureUsage,
};

const SIZE: u32 = 64;
const BLACK: u32 = 0xff00_0000;

/// Lit straight on with no ambient and a black background, so faces facing the camera come out
/// in exactly their color.
fn flat_environment() -> Environment {
    Environment {
        light_direction: -Vec3::Z,
        light_color: [1.0; 3],
        ambient: [0.0; 3],
        clear_color: [0.0, 0.0, 0.0, 1.0],
    }
}

struct Harness {
    device: &'static Device,
    renderer: Renderer,
    cube: MeshHandle,
    target: Texture,
    readback: Buffer,
}

impl Harness {
    fn new() -> Self {
        let device = hardware_device();
        let mut renderer = Renderer::new(
            device,
            &RendererDesc {
                color_format: Format::Rgba8Unorm,
                frames_in_flight: 2,
                width: SIZE,
                height: SIZE,
            },
        )
        .unwrap();
        renderer.environment = flat_environment();
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
                clear_value: Some(ClearValue::Color([0.0, 0.0, 0.0, 1.0])),
                ..TextureDesc::new_2d(
                    SIZE,
                    SIZE,
                    Format::Rgba8Unorm,
                    TextureUsage::RENDER_TARGET | TextureUsage::COPY_SRC,
                )
            })
            .unwrap();
        let readback = readback(device, SIZE as u64 * 256);
        Self {
            device,
            renderer,
            cube,
            target,
            readback,
        }
    }

    /// Renders `world` and returns the pixels (RGBA8 as little-endian u32) and stats.
    fn render(&mut self, world: &World) -> (Vec<u32>, RenderStats) {
        let device = self.device;
        let view = device
            .create_render_target_view(&self.target, &RenderTargetViewDesc::default())
            .unwrap();
        let mut stats = None;
        run(device, |list| {
            list.texture_barriers(&[TextureBarrier::new(
                &self.target,
                TextureState::UNDEFINED,
                TextureState::RENDER_TARGET,
            )]);
            // SAFETY: slot 0's previous work (if any) was waited for at the end of the last call.
            stats = Some(unsafe { self.renderer.render(device, list, 0, &view, world).unwrap() });
            list.texture_barriers(&[TextureBarrier::new(
                &self.target,
                TextureState::RENDER_TARGET,
                TextureState::COPY_SRC,
            )]);
            list.copy_texture_to_buffer(
                &self.target,
                &self.readback,
                &BufferTextureCopy::whole_mip(self.target.desc(), 0, 0, 0),
            );
            // The next call starts from UNDEFINED, which legacy barriers take as the common state.
            list.texture_barriers(&[TextureBarrier::new(
                &self.target,
                TextureState::COPY_SRC,
                TextureState::PRESENT,
            )]);
        });
        let pixels = read_words(&self.readback, 0, SIZE as usize * 64);
        assert_eq!(device.validation_errors(), 0);
        (pixels, stats.unwrap())
    }
}

fn pixel(pixels: &[u32], x: u32, y: u32) -> u32 {
    pixels[(y * 64 + x) as usize]
}

fn camera(world: &mut World, z: f32) {
    world.spawn((
        Transform::from_translation(Vec3::new(0.0, 0.0, z)),
        Camera::default(),
    ));
}

#[test]
fn draws_a_cube_in_front_of_the_camera() {
    let mut h = Harness::new();
    let mut world = World::new();
    camera(&mut world, 3.0);
    world.spawn((
        Transform::IDENTITY,
        MeshRenderer {
            mesh: h.cube,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    let (pixels, stats) = h.render(&world);
    assert_eq!(
        stats,
        RenderStats {
            instances: 1,
            draws: 1
        }
    );
    assert_eq!(pixel(&pixels, 32, 32), 0xff00_00ff, "the red front face");
    assert_eq!(pixel(&pixels, 1, 1), BLACK, "background");
}

#[test]
fn instances_of_one_mesh_share_a_draw_and_depth_sorts_them() {
    let mut h = Harness::new();
    let mut world = World::new();
    camera(&mut world, 5.0);
    let colors = [[0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]];
    for (x, color) in [-1.0, 1.0].into_iter().zip(colors) {
        world.spawn((
            Transform::from_translation(Vec3::new(x, 0.0, 0.0)),
            MeshRenderer {
                mesh: h.cube,
                color,
            },
        ));
    }
    // A white cube behind the left one, partly hidden by it.
    world.spawn((
        Transform {
            translation: Vec3::new(-1.0, 0.0, -2.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::splat(3.0),
        },
        MeshRenderer {
            mesh: h.cube,
            color: [1.0, 1.0, 1.0, 1.0],
        },
    ));
    let (pixels, stats) = h.render(&world);
    assert_eq!(
        stats,
        RenderStats {
            instances: 3,
            draws: 1
        }
    );
    // Cube centers project to x ≈ 32 ± 12.3.
    assert_eq!(
        pixel(&pixels, 20, 32),
        0xff00_ff00,
        "green, in front of white"
    );
    assert_eq!(pixel(&pixels, 44, 32), 0xffff_0000, "blue");
    assert_eq!(
        pixel(&pixels, 8, 32),
        0xffff_ffff,
        "the white cube around green"
    );
}

#[test]
fn without_a_camera_only_clears() {
    let mut h = Harness::new();
    let mut world = World::new();
    world.spawn((
        Transform::IDENTITY,
        MeshRenderer {
            mesh: h.cube,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    let (pixels, stats) = h.render(&world);
    assert_eq!(stats, RenderStats::default());
    assert!(pixels.iter().all(|&p| p == BLACK));
}

#[test]
fn renders_repeatedly_into_the_same_slot() {
    let mut h = Harness::new();
    let mut world = World::new();
    camera(&mut world, 3.0);
    let cube = world.spawn((
        Transform::IDENTITY,
        MeshRenderer {
            mesh: h.cube,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    for i in 0..3 {
        let color = [0.0, 0.0, 0.0, 1.0].map(|_| 0.0);
        let mut c = color;
        c[i] = 1.0;
        c[3] = 1.0;
        world.get_mut::<MeshRenderer>(cube).unwrap().color = c;
        let (pixels, _) = h.render(&world);
        assert_eq!(pixel(&pixels, 32, 32), 0xff00_0000 | (0xff << (8 * i)));
    }
}

#[test]
fn a_released_handle_stays_dead_after_its_slot_is_reused() {
    let mut h = Harness::new();
    let (vertices, indices) = cube();
    let mesh = MeshData {
        vertices: &vertices,
        indices: &indices,
    };
    let old = h.cube;
    // SAFETY: nothing has drawn the mesh yet.
    unsafe { h.renderer.release_mesh(old) };
    assert_eq!(h.renderer.mesh_count(), 0);
    let new = h.renderer.upload_mesh(h.device, &mesh).unwrap();
    assert_ne!(new, old);
    assert_eq!(h.renderer.mesh_count(), 1);

    let mut world = World::new();
    camera(&mut world, 3.0);
    let entity = world.spawn((
        Transform::IDENTITY,
        MeshRenderer {
            mesh: old,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    let (pixels, stats) = h.render(&world);
    assert_eq!(stats.draws, 0, "the old handle draws nothing");
    assert!(pixels.iter().all(|&p| p == BLACK));

    world.get_mut::<MeshRenderer>(entity).unwrap().mesh = new;
    let (pixels, stats) = h.render(&world);
    assert_eq!(stats.draws, 1);
    assert_eq!(pixel(&pixels, 32, 32), 0xff00_00ff);
}
