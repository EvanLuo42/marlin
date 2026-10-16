//! A field of spinning cubes in an ECS world, drawn by marlin-render: the first slice through
//! the whole stack (app, input, RHI, jobs, ECS, math, render) as a no_std game binary.
//!
//! Temporary, like the renderer it shows: it goes once the framework drives worlds and frames.
//!
//! Right mouse button + mouse, or the right stick: look. WASD or the left stick: move. Q/E or
//! the triggers: down/up. Shift: faster. Space: pause the spinning. V: toggle vsync.
//! F11 or Alt+Enter: borderless fullscreen. Esc or gamepad Start: quit.
#![no_std]
#![no_main]

use core::fmt::Write;
use core::time::Duration;

use marlin_app::{App, Event, Surface, WindowDesc};
use marlin_diag::{FixedBuf, chain, info};
use marlin_ecs::{Entity, World};
use marlin_input::{GamepadButton, Input, InputState, Key, MouseButton};
use marlin_jobs::{Config, JobSystem};
use marlin_math::scalar::{cos, sin};
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::{Camera, MeshData, MeshRenderer, Renderer, RendererDesc, cube};
use marlin_rhi::{
    DeviceDesc, FramesInFlight, Instance, InstanceDesc, PresentMode, PresentStatus, SurfaceHandle,
    SwapchainDesc, TextureBarrier, TextureState,
};
use marlin_time::Instant;

marlin_rt::game_entry!(demo_main);

fn demo_main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(error) => {
            marlin_diag::error!("demo_cubes: {}", chain(&error));
            1
        }
    }
}

// Each message names only the failing layer; `chain` appends the cause.
#[derive(Debug, marlin_error::Error)]
enum Failure {
    #[error("platform")]
    Platform(#[from] marlin_app::Error),
    #[error("gpu")]
    Gpu(#[from] marlin_rhi::Error),
}

/// Spins an entity about `axis` at `speed` radians per second.
struct Spin {
    axis: Vec3,
    speed: f32,
    phase: f32,
}

/// The free camera's orientation; its position lives in the camera entity's `Transform`.
struct FlyCamera {
    entity: Entity,
    yaw: f32,
    pitch: f32,
}

const GRID: i32 = 24;
const SPACING: f32 = 2.5;

fn run() -> Result<(), Failure> {
    let mut app = App::new()?;
    let window = app.create_window(&WindowDesc {
        title: "Marlin cubes",
        ..WindowDesc::default()
    })?;
    let mut input = Input::new(&app)?;
    let jobs = JobSystem::new(Config::default())?;

    let instance = Instance::new(&InstanceDesc::default())?;
    let device = instance.create_device(&DeviceDesc::default())?;
    info!(
        "adapter: {} ({:?})",
        device.adapter_info().name(),
        device.features()
    );

    let Surface::Win32 { hwnd, hinstance } = window.surface() else {
        unreachable!("only Win32 windows exist on this platform")
    };
    let size = window.size();
    // SAFETY: the window outlives the swapchain: locals drop in reverse order.
    let mut swapchain = unsafe {
        device.create_swapchain(
            SurfaceHandle::Win32 { hwnd, hinstance },
            &SwapchainDesc {
                width: size.width,
                height: size.height,
                ..SwapchainDesc::default()
            },
        )
    }?;
    let mut frames = FramesInFlight::new(&device, 2)?;
    let mut renderer = Renderer::new(
        &device,
        &RendererDesc {
            color_format: swapchain.format(),
            frames_in_flight: frames.count(),
            width: size.width,
            height: size.height,
        },
    )?;
    let (vertices, indices) = cube();
    let cube = renderer.upload_mesh(
        &device,
        &MeshData {
            vertices: &vertices,
            indices: &indices,
        },
    )?;

    let mut world = World::new();
    populate(&mut world, cube);
    let mut fly = FlyCamera {
        entity: world.spawn((
            Transform::from_translation(Vec3::new(0.0, 6.0, 40.0)),
            Camera::default(),
        )),
        yaw: 0.0,
        pitch: -0.15,
    };
    info!("{} entities", world.entity_count());

    let mut vsync = true;
    let mut paused = false;
    let mut minimized = false;
    let mut time = 0.0f32;
    let mut last = Instant::now();
    let mut fps_window = (Instant::now(), 0u32);

    'main: loop {
        let timeout = if minimized {
            Some(Duration::from_millis(100))
        } else {
            Some(Duration::ZERO)
        };
        for &event in app.pump_timeout(timeout) {
            match event {
                Event::CloseRequested(_) | Event::Quit => break 'main,
                Event::Resized { size, .. } => {
                    swapchain.resize(size.width, size.height)?;
                    // SAFETY: resizing the swapchain waited for the queue to go idle.
                    unsafe { renderer.resize(&device, size.width, size.height)? };
                }
                Event::Minimized(_) => minimized = true,
                Event::Restored(_) => minimized = false,
                _ => {}
            }
        }

        input.update();
        let state = input.state();
        if state.key_pressed(Key::ESCAPE) || state.gamepad(0).pressed(GamepadButton::Start) {
            break;
        }
        if state.key_pressed(Key::F11) || (state.alt() && state.key_pressed(Key::ENTER)) {
            window.toggle_fullscreen()?;
        }
        if state.key_pressed(Key::V) {
            vsync = !vsync;
            swapchain.set_present_mode(if vsync {
                PresentMode::Fifo
            } else {
                PresentMode::Immediate
            });
            info!("vsync {}", if vsync { "on" } else { "off" });
        }
        if state.key_pressed(Key::SPACE) {
            paused = !paused;
        }

        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f32().min(0.1);
        last = now;
        if !paused {
            time += dt;
        }
        fly_camera(&mut world, &mut fly, state, dt);
        world.par_for_each::<(&mut Transform, &Spin), _>(&jobs, 64, |(transform, spin)| {
            transform.rotation = Quat::from_axis_angle(spin.axis, spin.phase + spin.speed * time);
        });

        if minimized {
            continue;
        }

        let image = swapchain.acquire(None)?;
        let texture = swapchain.texture(image);
        let slot = frames.index();
        let list = frames.begin()?;
        list.texture_barriers(&[TextureBarrier::new(
            texture,
            TextureState::PRESENT,
            TextureState::RENDER_TARGET,
        )]);
        // SAFETY: `begin` waited for the slot's previous frame; the list is submitted below,
        // before the slot comes round again; the renderer outlives the frames (`wait_idle`).
        unsafe { renderer.render(&device, list, slot, swapchain.view(image), &world)? };
        list.texture_barriers(&[TextureBarrier::new(
            texture,
            TextureState::RENDER_TARGET,
            TextureState::PRESENT,
        )]);
        frames.submit(device.queue())?;
        if swapchain.present()? == PresentStatus::Occluded {
            app.pump_timeout(Some(Duration::from_millis(16)));
        }

        fps_window.1 += 1;
        let elapsed = now.duration_since(fps_window.0);
        if elapsed >= Duration::from_secs(1) {
            let mut title = FixedBuf::<64>::new();
            let fps = fps_window.1 as f32 / elapsed.as_secs_f32();
            let _ = write!(title, "Marlin cubes  {fps:.0} fps");
            window.set_title(title.as_str());
            fps_window = (now, 0);
        }
    }

    frames.wait_idle()?;
    Ok(())
}

/// A ground slab and a grid of spinning cubes in a slow rainbow.
fn populate(world: &mut World, cube: marlin_render::MeshHandle) {
    world.spawn((
        Transform {
            translation: Vec3::new(0.0, -2.0, 0.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::new(
                GRID as f32 * SPACING + 4.0,
                0.5,
                GRID as f32 * SPACING + 4.0,
            ),
        },
        MeshRenderer {
            mesh: cube,
            color: [0.35, 0.36, 0.38, 1.0],
        },
    ));
    let half = (GRID - 1) as f32 * SPACING * 0.5;
    for z in 0..GRID {
        for x in 0..GRID {
            let (fx, fz) = (x as f32, z as f32);
            let hue = (fx + fz) / (2 * GRID) as f32;
            let axis = Vec3::new(sin(fx * 1.3), 1.0, cos(fz * 0.7)).normalize();
            world.spawn((
                Transform::from_translation(Vec3::new(
                    fx * SPACING - half,
                    0.6 * sin(fx * 0.5 + fz * 0.3),
                    fz * SPACING - half,
                )),
                MeshRenderer {
                    mesh: cube,
                    color: rainbow(hue),
                },
                Spin {
                    axis,
                    speed: 0.5 + (x * 7 + z * 13) as f32 % 10.0 * 0.15,
                    phase: fx * 0.37 + fz * 0.61,
                },
            ));
        }
    }
}

/// A saturated color for `t` in 0..1, in linear space.
fn rainbow(t: f32) -> [f32; 4] {
    const TAU: f32 = core::f32::consts::TAU;
    let channel = |phase: f32| {
        let s = 0.5 + 0.5 * sin(t * TAU + phase * TAU / 3.0);
        s * s
    };
    [channel(0.0), channel(1.0), channel(2.0), 1.0]
}

fn fly_camera(world: &mut World, fly: &mut FlyCamera, state: &InputState, dt: f32) {
    let pad = state.gamepad(0);
    let mut look = pad.right_stick().map(|v| v * 2.5 * dt);
    if state.mouse().down(MouseButton::Right) {
        let [dx, dy] = state.mouse().delta();
        look[0] += dx * 0.003;
        look[1] -= dy * 0.003;
    }
    fly.yaw -= look[0];
    fly.pitch = (fly.pitch + look[1]).clamp(-1.5, 1.5);
    let rotation = Quat::from_euler_yxz(fly.yaw, fly.pitch, 0.0);

    let key = |k| if state.key_down(k) { 1.0 } else { 0.0 };
    let [stick_x, stick_y] = pad.left_stick();
    let right = key(Key::D) - key(Key::A) + stick_x;
    let forward = key(Key::W) - key(Key::S) + stick_y;
    let up = key(Key::E) - key(Key::Q) + pad.right_trigger() - pad.left_trigger();
    let speed = if state.shift() { 30.0 } else { 10.0 };
    let movement = rotation.mul_vec3(Vec3::new(right, 0.0, -forward)) + Vec3::Y * up;

    if let Some(transform) = world.get_mut::<Transform>(fly.entity) {
        transform.rotation = rotation;
        transform.translation += movement * (speed * dt);
    }
}
