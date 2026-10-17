//! The forward renderer on the hardware adapter, with validation on: scenes rendered offscreen
//! and read back.
#![cfg(marlin_platform = "windows")]

use std::sync::OnceLock;

use marlin_ecs::World;
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::{
    Camera, Environment, MeshData, MeshHandle, MeshRenderer, RenderStats, Renderer, RendererDesc,
    cube,
};
use marlin_rhi::{
    AdapterPreference, Buffer, BufferDesc, BufferTextureCopy, BufferUsage, ClearValue, Device,
    DeviceDesc, Format, Instance, InstanceDesc, MemoryLocation, RenderTargetViewDesc, Texture,
    TextureBarrier, TextureDesc, TextureState, TextureUsage,
};

const SIZE: u32 = 64;
const BLACK: u32 = 0xff00_0000;

/// The hardware adapter with validation on, shared by every test in this binary and never
/// dropped: tearing down `Device`s while other threads use the same adapter can deadlock the
/// NVIDIA driver (see the RHI tests' `support::devices`).
fn hardware_device() -> &'static Device {
    static DEVICE: OnceLock<Device> = OnceLock::new();
    DEVICE.get_or_init(|| {
        Instance::new(&InstanceDesc {
            validation: true,
            ..InstanceDesc::default()
        })
        .unwrap()
        .create_device(&DeviceDesc {
            adapter: AdapterPreference::HighPerformance,
            ..DeviceDesc::default()
        })
        .unwrap()
    })
}

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
        let readback = device
            .create_buffer(&BufferDesc {
                size: SIZE as u64 * 256,
                usage: BufferUsage::COPY_DST,
                memory: MemoryLocation::Readback,
            })
            .unwrap();
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
        let mut pool = device.create_command_pool().unwrap();
        let mut list = device.create_command_list(&pool).unwrap();
        let fence = device.create_fence(0).unwrap();
        list.begin(&mut pool).unwrap();
        list.texture_barriers(&[TextureBarrier::new(
            &self.target,
            TextureState::UNDEFINED,
            TextureState::RENDER_TARGET,
        )]);
        // SAFETY: slot 0's previous work (if any) was waited for at the end of the last call.
        let stats = unsafe {
            self.renderer
                .render(device, &mut list, 0, &view, world)
                .unwrap()
        };
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
        list.end().unwrap();
        // SAFETY: everything the list uses outlives the wait below.
        unsafe { device.queue().submit(&[&list], Some((&fence, 1))).unwrap() };
        fence.wait(1, None).unwrap();

        let mut bytes = vec![0; SIZE as usize * 256];
        // SAFETY: the copy has finished.
        unsafe { self.readback.read(0, &mut bytes) };
        let pixels = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&p| u32::from_le_bytes(p))
            .collect();
        assert_eq!(device.validation_errors(), 0);
        (pixels, stats)
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
