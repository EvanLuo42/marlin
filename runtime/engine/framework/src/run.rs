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
use marlin_profile::profile_scope;
use marlin_render_graph::{FrameContext, FrameContextDesc, GpuTimeline};
use marlin_rhi::{
    DeviceDesc, Instance, InstanceDesc, PresentMode, PresentStatus, SurfaceHandle, Swapchain,
    SwapchainDesc,
};
use marlin_time::Instant;

use crate::boot::{Boot, R_STATS, R_STATS_LOG, R_VSYNC, WINDOW_HEIGHT, WINDOW_WIDTH};
use crate::delta::{RenderDelta, RenderSide, RenderSideError};
use crate::engine::{Engine, EngineError};
use crate::hud::{Hud, HudInfo, Snapshot};
use crate::loaders::engine_loaders;
use crate::shaders::{ShaderError, load_default_resolve, load_engine_shaders, load_ui_font};

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
/// Keys: F11 or Alt+Enter, borderless fullscreen; V, vsync; F3, the stats overlay.
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
    set_vsync(&mut swapchain, vsync);
    // The passes' GPU times, for the profiler.
    static TIMELINE: GpuTimeline = GpuTimeline::new();
    marlin_profile::set_gpu_timings(Some(&TIMELINE));
    let mut frames = FrameContext::new(
        &device,
        &FrameContextDesc {
            timeline: Some(&TIMELINE),
            // Independent passes run side by side (0007 §4.8).
            overlap: true,
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

    // The stats overlay, made the first time it is shown; its font has an asset server of its
    // own, since the engine's is the simulation's while a frame renders.
    let mut show_stats = R_STATS.get();
    let mut hud: Option<Hud> = None;
    let mut hud_assets = marlin_asset::AssetServer::new(engine_loaders(&engine.streamer));
    let mut hud_font = None;
    let adapter = alloc::string::String::from(device.adapter_info().name());

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
            set_vsync(&mut swapchain, vsync);
        }
        if state.key_pressed(Key::F3) {
            show_stats = !show_stats;
        }
        if show_stats && hud.is_none() {
            let made = engine
                .assets
                .get(&library)
                .ok_or(ShaderError::Load(marlin_asset::AssetError::NotFound))
                .map_err(|e| marlin_diag::warn!("stats overlay: {}", marlin_diag::chain(&e)))
                .ok()
                .and_then(|library| {
                    Hud::new(&device, library, swapchain.format())
                        .map_err(|e| {
                            marlin_diag::warn!("stats overlay: {}", marlin_diag::chain(&e))
                        })
                        .ok()
                });
            let font = load_ui_font(&mut hud_assets, &boot.vfs, Some(&jobs))
                .map_err(|e| marlin_diag::warn!("stats overlay font: {e:?}"))
                .ok();
            match (made, font) {
                (Some(h), Some(f)) => (hud, hud_font) = (Some(h), Some(f)),
                // Not to be tried every frame.
                _ => show_stats = false,
            }
        }
        let now = Instant::now();
        let interval = now.duration_since(last).as_secs_f32();
        let dt = interval.min(0.1);
        last = now;
        if let Some(h) = hud.as_mut().filter(|_| show_stats) {
            h.push_interval(interval);
        }
        // The overlay of this frame: its panel, in the font, for the window as it is.
        let overlay = match (&mut hud, &hud_font) {
            (Some(h), Some(font)) if show_stats => {
                let size = window.size();
                let stats = frames.stats();
                Some(Overlay {
                    hud: h,
                    font: hud_assets.get(font).map(|f| f.view()),
                    scale: window.scale_factor(),
                    info: HudInfo {
                        adapter: &adapter,
                        width: size.width,
                        height: size.height,
                        vsync,
                        passes: stats.map_or(0, |s| s.passes),
                        lists: stats.map_or(0, |s| s.lists),
                    },
                })
            }
            _ => None,
        };

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
                    overlay,
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
            if R_STATS_LOG.get() || (show_stats && hud.is_some()) {
                let snapshot = Snapshot::take(fps);
                if R_STATS_LOG.get() {
                    snapshot.log();
                }
                if let Some(h) = hud.as_mut().filter(|_| show_stats) {
                    h.set_snapshot(snapshot);
                }
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
    if let Some(h) = &hud {
        h.forget(&mut frames);
    }
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
    overlay: Option<Overlay<'_>>,
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
    let overlay = match overlay {
        Some(Overlay {
            hud,
            font: Some(font),
            scale,
            info,
        }) => {
            profile_scope!("Overlay");
            let frame = hud.frame(device, frames, font, scale, &info, Some(jobs))?;
            Some((&*hud, frame))
        }
        _ => None,
    };
    // The swapchain image is imported like any texture: the graph remembers it was presented.
    {
        profile_scope!("Execute");
        frames.execute_on(device, Some(jobs), |g, _| {
            let target = g.import_texture("Swapchain", texture);
            let mut target = render.add_passes(g, target);
            if let Some((hud, frame)) = &overlay {
                target = hud.add_passes(g, frame, target);
            }
            g.present(target);
        })?;
    }
    profile_scope!("Present");
    Ok(swapchain.present()?)
}

/// The stats overlay of a frame: what it shows, in which font, at which scale.
struct Overlay<'a> {
    hud: &'a mut Hud,
    /// `None` while the font is not loaded: nothing is drawn.
    font: Option<marlin_font::FontView<'a>>,
    scale: f32,
    info: HudInfo<'a>,
}

/// Presents at vertical blank or as soon as a frame is done. Without vsync a frame more may be
/// queued: with two, the GPU idles while the CPU waits for the presentation engine to take the
/// last one.
fn set_vsync(swapchain: &mut Swapchain, vsync: bool) {
    if vsync {
        swapchain.set_present_mode(PresentMode::Fifo);
        swapchain.set_max_frame_latency(2);
    } else {
        swapchain.set_present_mode(PresentMode::Immediate);
        swapchain.set_max_frame_latency(3);
    }
}
