//! The windowed game loop.
//!
//! Each turn of the loop renders the previous simulation frame's [`RenderDelta`] on this
//! thread while a job simulates the next one, so the CPU sides overlap (Game N+1 ∥
//! Render N, with the GPU on N-1 through two frames in flight).

use core::cell::Cell;
use core::fmt::Write;
use core::time::Duration;

use marlin_app::{App, Event, Surface, WindowDesc};
use marlin_diag::{FixedBuf, info};
use marlin_input::{GamepadButton, Input, Key};
use marlin_jobs::{Config, JobSystem};
use marlin_profile::{GpuZone, profile_scope};
use marlin_render_graph::{FrameContext, FrameContextDesc, GpuTimeline};
use marlin_rhi::{
    DeviceDesc, Instance, InstanceDesc, PresentMode, PresentStatus, SurfaceHandle, Swapchain,
    SwapchainDesc,
};
use marlin_time::Instant;

use crate::boot::{Boot, R_STATS, R_VSYNC, WINDOW_HEIGHT, WINDOW_WIDTH};
use crate::delta::{RenderDelta, RenderSide, RenderSideError};
use crate::engine::{Engine, EngineError};
use crate::shaders::{ShaderError, load_default_resolve, load_engine_shaders};

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
    #[error("render passes")]
    Render(#[from] RenderSideError),
}

/// What a turn of the loop asks of its caller.
enum Step {
    Continue,
    Quit,
    /// The window is hidden behind another: idle a moment before the next turn.
    Occluded,
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
    let resolve = load_default_resolve(&mut engine.assets, &boot.vfs, Some(&jobs))?;
    let shaders = engine
        .assets
        .get(&library)
        .ok_or(ShaderError::Load(marlin_asset::AssetError::NotFound))?;
    let resolve = engine
        .assets
        .get(&resolve)
        .ok_or(ShaderError::Load(marlin_asset::AssetError::NotFound))?;
    let mut render = RenderSide::new(
        &device,
        shaders,
        resolve,
        swapchain.format(),
        engine.scene_ids(),
    )?;
    engine.load_level(boot.level);

    let mut pending: Option<RenderDelta> = None;
    let minimized = Cell::new(false);
    let mut last = Instant::now();
    let mut fps_window = (Instant::now(), 0u32);
    // 0: loading; 1: the engine is ready (its uploads ride in the next delta); 2: reported.
    let mut reported = 0;

    // One turn of the loop: the events since the last, then simulate and render. Also run from
    // the platform's tick while the OS holds the pump in a window drag.
    let mut frame = |events: &[Event]| -> Result<Step, RunError> {
        for &event in events {
            match event {
                Event::CloseRequested(_) | Event::Quit => return Ok(Step::Quit),
                Event::Resized { size, .. } => {
                    // The images go: the graph forgets them first.
                    frames.wait_idle()?;
                    forget_images(&mut frames, &swapchain);
                    swapchain.resize(size.width, size.height)?;
                }
                Event::Minimized(_) => minimized.set(true),
                Event::Restored(_) => minimized.set(false),
                _ => {}
            }
        }
        input.update();
        let state = input.state();
        if state.key_pressed(Key::ESCAPE) || state.gamepad(0).pressed(GamepadButton::Start) {
            return Ok(Step::Quit);
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
            s.spawn(|| {
                profile_scope!("Simulate");
                next = Some(engine_ref.frame(dt, Some(state), Some(jobs_ref)));
            });
            if let Some(delta) = pending.take()
                && !minimized.get()
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
        let occluded = presented? == Some(PresentStatus::Occluded);
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
        marlin_profile::frame_mark();
        fps_window.1 += 1;
        let elapsed = now.duration_since(fps_window.0);
        if elapsed >= Duration::from_secs(1) {
            let mut text = FixedBuf::<96>::new();
            let fps = fps_window.1 as f32 / elapsed.as_secs_f32();
            let _ = write!(text, "{title}  {fps:.0} fps");
            window.set_title(text.as_str());
            if R_STATS.get() {
                log_stats(fps);
            }
            fps_window = (now, 0);
        }
        Ok(if occluded {
            Step::Occluded
        } else {
            Step::Continue
        })
    };
    let mut tick_step = Step::Continue;
    loop {
        let timeout = Some(if minimized.get() {
            Duration::from_millis(100)
        } else {
            Duration::ZERO
        });
        let mut tick_error = None;
        let events = app.pump_with(timeout, |events| {
            if tick_error.is_none() {
                match frame(events) {
                    Ok(Step::Quit) => tick_step = Step::Quit,
                    Ok(_) => {}
                    Err(e) => tick_error = Some(e),
                }
            }
        });
        if let Some(e) = tick_error {
            return Err(e);
        }
        if matches!(tick_step, Step::Quit) {
            break;
        }
        match frame(events)? {
            Step::Quit => break,
            Step::Occluded => {
                app.pump_timeout(Some(Duration::from_millis(16)));
            }
            Step::Continue => {}
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
    profile_scope!("Render");
    {
        profile_scope!("Feedback");
        render.collect_feedback(frames);
    }
    {
        profile_scope!("Apply");
        render.apply(device, delta)?;
    }
    let image = {
        profile_scope!("Acquire");
        swapchain.acquire(None)?
    };
    let texture = swapchain.texture(image);
    // The swapchain image is imported like any texture: the graph remembers it was presented.
    {
        profile_scope!("Execute");
        frames.execute_on(device, Some(jobs), |g, _| {
            let target = g.import_texture("Swapchain", texture);
            let target = render.add_passes(g, target);
            g.present(target);
        })?;
    }
    profile_scope!("Present");
    Ok(swapchain.present()?)
}

/// Logs `fps` and, over the profiler's history, the mean frame, the GPU's span per frame (first
/// pass start to last pass end) and busy time per queue, and the costliest CPU zones and GPU
/// passes.
fn log_stats(fps: f32) {
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    marlin_profile::with_history(|h| {
        let n = h.frames().len().max(1) as u32;
        let wall = h.frames().map(|f| f.duration()).sum::<Duration>() / n;
        let (mut span, mut busy) = (Duration::ZERO, [Duration::ZERO; 3]);
        for f in h.frames() {
            let zones = f.gpu_zones();
            if let (Some(lo), Some(hi)) = (
                zones.iter().map(|z| z.start).min(),
                zones.iter().map(|z| z.end).max(),
            ) {
                span += GpuZone {
                    start: lo,
                    end: hi,
                    ..zones[0]
                }
                .duration();
            }
            for z in zones {
                busy[(z.queue as usize).min(2)] += z.duration();
            }
        }
        info!(
            "{fps:.0} fps, {:.3} ms/frame; gpu span {:.3} ms, busy {:.3} / {:.3} / {:.3} ms \
             (graphics / compute / copy)",
            ms(wall),
            ms(span / n),
            ms(busy[0] / n),
            ms(busy[1] / n),
            ms(busy[2] / n),
        );
        for (label, stats) in [("cpu", h.zone_stats()), ("gpu", h.gpu_zone_stats())] {
            for s in stats.iter().take(12) {
                info!(
                    "  {label} {:>8.3} ms  {:>3}x  {}",
                    ms(s.per_frame.avg),
                    s.calls / u64::from(s.frames.max(1)),
                    s.name
                );
            }
        }
    });
}
