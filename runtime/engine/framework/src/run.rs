//! The windowed game loop.
//!
//! Each turn of the loop renders the previous simulation frame's [`RenderDelta`] on this
//! thread while a job simulates the next one, so the CPU sides overlap (0004 §3: Game N+1 ∥
//! Render N, with the GPU on N-1 through two frames in flight).

use core::fmt::Write;
use core::time::Duration;

use marlin_app::{App, Event, Surface, WindowDesc};
use marlin_diag::{FixedBuf, info};
use marlin_input::{GamepadButton, Input, Key};
use marlin_jobs::{Config, JobSystem};
use marlin_render::{Renderer, RendererDesc};
use marlin_rhi::{
    DeviceDesc, FramesInFlight, Instance, InstanceDesc, PresentMode, PresentStatus, SurfaceHandle,
    SwapchainDesc, TextureBarrier, TextureState,
};
use marlin_time::Instant;

use crate::boot::{Boot, R_VSYNC, WINDOW_HEIGHT, WINDOW_WIDTH};
use crate::delta::{RenderDelta, RenderSide};
use crate::engine::{Engine, EngineError};

#[derive(Debug, marlin_error::Error)]
pub enum RunError {
    #[error("platform")]
    Platform(#[from] marlin_app::Error),
    #[error("gpu")]
    Gpu(#[from] marlin_rhi::Error),
    #[error("engine")]
    Engine(#[from] EngineError),
}

/// Runs the game in a window until it is closed (or Esc / gamepad Start).
///
/// Keys: F11 or Alt+Enter, borderless fullscreen; V, vsync.
pub fn run(boot: &Boot, title: &str) -> Result<(), RunError> {
    let mut app = App::new()?;
    let window = app.create_window(&WindowDesc {
        title,
        width: WINDOW_WIDTH.get() as u32,
        height: WINDOW_HEIGHT.get() as u32,
        ..WindowDesc::default()
    })?;
    let mut input = Input::new(&app)?;
    let jobs = JobSystem::new(Config::default())?;

    let instance = Instance::new(&InstanceDesc::default())?;
    let device = instance.create_device(&DeviceDesc::default())?;
    info!("adapter: {}", device.adapter_info().name());
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
    let mut vsync = R_VSYNC.get();
    swapchain.set_present_mode(if vsync {
        PresentMode::Fifo
    } else {
        PresentMode::Immediate
    });
    let mut frames = FramesInFlight::new(&device, 2)?;
    let renderer = Renderer::new(
        &device,
        &RendererDesc {
            color_format: swapchain.format(),
            frames_in_flight: frames.count(),
            width: size.width,
            height: size.height,
        },
    )?;
    let mut render = RenderSide::new(renderer, frames.count());

    let mut engine = Engine::new(&boot.vfs)?;
    engine.load_level(boot.level);

    let mut pending: Option<RenderDelta> = None;
    let mut minimized = false;
    let mut last = Instant::now();
    let mut fps_window = (Instant::now(), 0u32);
    // 0: loading; 1: the engine is ready (its uploads ride in the next delta); 2: reported.
    let mut reported = 0;

    'main: loop {
        let timeout = Some(if minimized {
            Duration::from_millis(100)
        } else {
            Duration::ZERO
        });
        for &event in app.pump_timeout(timeout) {
            match event {
                Event::CloseRequested(_) | Event::Quit => break 'main,
                Event::Resized { size, .. } => {
                    swapchain.resize(size.width, size.height)?;
                    // SAFETY: resizing the swapchain waited for the queue to go idle.
                    unsafe { render.renderer.resize(&device, size.width, size.height)? };
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
        }
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f32().min(0.1);
        last = now;

        // Simulate the next frame on a job while this thread renders the last one.
        let mut next = None;
        let mut presented: Result<Option<PresentStatus>, RunError> = Ok(None);
        let engine_ref = &mut engine;
        let jobs_ref = &jobs;
        jobs.scope(|s| {
            s.spawn(|| next = Some(engine_ref.frame(dt, Some(state), Some(jobs_ref))));
            if let Some(delta) = pending.take()
                && !minimized
            {
                presented = render_frame(&device, &mut render, &mut swapchain, &mut frames, &delta)
                    .map(Some);
            }
        });
        if presented? == Some(PresentStatus::Occluded) {
            app.pump_timeout(Some(Duration::from_millis(16)));
        }
        pending = next;

        if reported == 1 {
            reported = 2;
            info!(
                "level loaded: {} entities, {} meshes",
                engine.world.entity_count(),
                render.mesh_count()
            );
        } else if reported == 0 && engine.is_ready() {
            reported = 1;
        }
        fps_window.1 += 1;
        let elapsed = now.duration_since(fps_window.0);
        if elapsed >= Duration::from_secs(1) {
            let mut text = FixedBuf::<96>::new();
            let fps = fps_window.1 as f32 / elapsed.as_secs_f32();
            let _ = write!(text, "{title}  {fps:.0} fps");
            window.set_title(text.as_str());
            fps_window = (now, 0);
        }
    }
    frames.wait_idle()?;
    Ok(())
}

fn render_frame(
    device: &marlin_rhi::Device,
    render: &mut RenderSide,
    swapchain: &mut marlin_rhi::Swapchain,
    frames: &mut FramesInFlight,
    delta: &RenderDelta,
) -> Result<PresentStatus, RunError> {
    render.apply(device, delta)?;
    let image = swapchain.acquire(None)?;
    let texture = swapchain.texture(image);
    let slot = frames.index();
    let list = frames.begin()?;
    list.texture_barriers(&[TextureBarrier::new(
        texture,
        TextureState::PRESENT,
        TextureState::RENDER_TARGET,
    )]);
    // SAFETY: `begin` waited for the slot's previous frame; the list is submitted below, before
    // the slot comes round again; the renderer outlives the frames (`wait_idle` on exit).
    unsafe { render.record(device, list, slot, swapchain.view(image))? };
    list.texture_barriers(&[TextureBarrier::new(
        texture,
        TextureState::RENDER_TARGET,
        TextureState::PRESENT,
    )]);
    frames.submit(device.queue())?;
    Ok(swapchain.present()?)
}
