//! The windowed game loop.
//!
//! Each turn of the loop renders the previous simulation frame's [`RenderDelta`] on this
//! thread while a job simulates the next one, so the CPU sides overlap (Game N+1 ∥
//! Render N, with the GPU on N-1 through two frames in flight).

use core::fmt::Write;
use core::time::Duration;

use marlin_app::{App, Event, Surface, WindowDesc};
use marlin_diag::{FixedBuf, info};
use marlin_input::{GamepadButton, Input, Key};
use marlin_jobs::{Config, JobSystem};
use marlin_render::{HDR_FORMAT, PostProcess, Renderer, RendererDesc, ScatterShader};
use marlin_render_graph::{FrameContext, FrameContextDesc, GpuTimeline};
use marlin_rhi::{
    DeviceDesc, Instance, InstanceDesc, PresentMode, PresentStatus, SurfaceHandle, Swapchain,
    SwapchainDesc,
};
use marlin_time::Instant;

use crate::boot::{Boot, R_VSYNC, WINDOW_HEIGHT, WINDOW_WIDTH};
use crate::delta::{RenderDelta, RenderSide};
use crate::engine::{Engine, EngineError};
use crate::shaders::{
    ShaderError, forward_shaders, load_engine_shaders, post_shaders, scatter_shader,
};

#[derive(Debug, marlin_error::Error)]
pub enum RunError {
    #[error("platform")]
    Platform(#[from] marlin_app::Error),
    #[error("gpu")]
    Gpu(#[from] marlin_rhi::Error),
    #[error("engine")]
    Engine(#[from] EngineError),
    #[error("engine shaders")]
    Shaders(#[from] ShaderError),
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
    // The passes' GPU times, for the profiler.
    static TIMELINE: GpuTimeline = GpuTimeline::new();
    marlin_profile::set_gpu_timings(Some(&TIMELINE));
    let mut frames = FrameContext::new(
        &device,
        &FrameContextDesc {
            timeline: Some(&TIMELINE),
            ..FrameContextDesc::default()
        },
    )?;

    let mut engine = Engine::new(&boot.vfs)?;
    // The renderer's shaders, cooked from content/engine like any asset.
    let library = load_engine_shaders(&mut engine.assets, &boot.vfs, Some(&jobs))?;
    let shaders = engine
        .assets
        .get(&library)
        .ok_or(ShaderError::Load(marlin_asset::AssetError::NotFound))?;
    let renderer = Renderer::new(
        &device,
        &RendererDesc {
            color_format: HDR_FORMAT,
            shaders: Some(forward_shaders(shaders)?),
        },
    )?;
    let post = PostProcess::new(&device, &post_shaders(shaders)?, swapchain.format())?;
    let scatter = ScatterShader::new(&device, scatter_shader(shaders)?)?;
    let mut render = RenderSide::new(renderer, post, scatter, engine.scene_ids());
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
                    // The images go: the graph forgets them first.
                    frames.wait_idle()?;
                    forget_images(&mut frames, &swapchain);
                    swapchain.resize(size.width, size.height)?;
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
                presented = render_frame(
                    &device,
                    jobs_ref,
                    &mut render,
                    &mut swapchain,
                    &mut frames,
                    &delta,
                )
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
    forget_images(&mut frames, &swapchain);
    marlin_profile::set_gpu_timings(None);
    Ok(())
}

/// Makes the frame context forget the swapchain's images, before they are recreated or dropped.
fn forget_images(frames: &mut FrameContext, swapchain: &Swapchain) {
    for i in 0..swapchain.image_count() {
        frames.forget_texture(swapchain.texture(i));
    }
}

fn render_frame(
    device: &marlin_rhi::Device,
    jobs: &JobSystem,
    render: &mut RenderSide,
    swapchain: &mut Swapchain,
    frames: &mut FrameContext,
    delta: &RenderDelta,
) -> Result<PresentStatus, RunError> {
    render.apply(device, delta)?;
    let image = swapchain.acquire(None)?;
    let texture = swapchain.texture(image);
    // The swapchain image is imported like any texture: the graph remembers it was presented.
    frames.execute_on(device, Some(jobs), |g, _| {
        let target = g.import_texture("Swapchain", texture);
        let (target, _) = render.add_passes(g, target);
        g.present(target);
    })?;
    Ok(swapchain.present()?)
}
