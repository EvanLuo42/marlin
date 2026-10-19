//! The windowed game loop.
//!
//! Each turn of the loop renders the previous simulation frame's [`RenderDelta`] on this
//! thread while a job simulates the next one, so the CPU sides overlap (Game N+1 ∥
//! Render N, with the GPU on N-1 through two frames in flight).

use core::cell::Cell;
use core::time::Duration;

use marlin_app::{App, Event, Surface, Window, WindowDesc};
use marlin_diag::info;
use marlin_input::Input;
use marlin_jobs::{Config, JobSystem};
use marlin_profile::profile_scope;
use marlin_render_graph::{FrameContext, FrameContextDesc, GpuTimeline};
use marlin_rhi::{
    DeviceDesc, Instance, InstanceDesc, PresentMode, PresentStatus, SurfaceHandle, Swapchain,
    SwapchainDesc,
};
use marlin_time::Instant;

use crate::boot::{Boot, R_VSYNC, WINDOW_HEIGHT, WINDOW_WIDTH};
use marlin_renderer::{RenderAssets, RenderDelta, WorldRenderer, WorldRendererError};
use marlin_world::{Modules, Transform, WorldKind};

use crate::dev::DevTools;
use crate::engine::{Engine, EngineError};
use crate::input::{
    HostInput, ViewportInput, apply_cursor, give_player_input, input_settings, player_zero,
};

#[derive(Debug, marlin_error::Error)]
pub enum RunError {
    #[error("platform")]
    Platform(#[from] marlin_app::Error),
    #[error("gpu")]
    Gpu(#[from] marlin_rhi::Error),
    #[error("engine")]
    Engine(#[from] EngineError),
    #[error("the world renderer")]
    Render(#[from] WorldRendererError),
    #[error("no render module: this build cannot draw")]
    NoRenderer,
}

/// What a turn of the loop asks of its caller.
enum Step {
    Continue,
    Quit,
    /// The window is hidden behind another: idle a moment before the next turn.
    Occluded,
}

/// Runs the game of `modules` (registered before `boot` read its configuration) in a window until
/// it is closed (or the host's quit command): a game world with the entry level, drawn by a
/// [`WorldRenderer`].
///
/// The host's commands are input actions (`content/engine/input/Host.masset`): F11 or Alt+Enter,
/// borderless fullscreen; and in development builds Esc or gamepad Start, quit; V, vsync; F3, the
/// stats overlay ([`dev`](crate::dev)). The rest reaches player 0 as the world's input settings
/// say ([`crate::input`]).
pub fn run(boot: &Boot, title: &str, modules: Modules) -> Result<(), RunError> {
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

    let mut engine = Engine::new(&boot.vfs, modules, Some(&jobs))?;
    let mut world = engine.create_world(WorldKind::Game, Some(&jobs));
    world.load_sublevel(boot.level, Transform::IDENTITY);
    // The world's renderer, made with the render module's engine assets.
    let render_assets = engine
        .resources()
        .get::<RenderAssets>()
        .ok_or(RunError::NoRenderer)?;
    let library = render_assets.library.clone();
    let mut render = WorldRenderer::new(
        &device,
        &engine.assets,
        render_assets,
        swapchain.format(),
        marlin_renderer::scene_ids(&world).ok_or(RunError::NoRenderer)?,
    )?;

    // The host's commands, and the window as one game viewport.
    let mut host_input = HostInput::load(&mut engine.assets, &boot.vfs, Some(&jobs));
    let mut viewport = ViewportInput::game();
    let mut cursor = None;

    let mut dev = DevTools::new(
        &boot.vfs,
        engine.modules().has("DevTools"),
        engine.loaders(),
        device.adapter_info().name(),
    );

    let mut pending: Option<RenderDelta> = None;
    let minimized = Cell::new(false);
    let mut last = Instant::now();
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
        let now = Instant::now();
        let interval = now.duration_since(last).as_secs_f32();
        let dt = interval.min(0.1);
        last = now;
        // Routing (0019 §9.2): the host's commands, then the window's one viewport, then player 0.
        let (commands, rest) = host_input.route(player_zero(state), interval);
        if commands.quit {
            return Ok(Step::Quit);
        }
        if commands.fullscreen {
            window.toggle_fullscreen()?;
        }
        if commands.vsync {
            vsync = !vsync;
            set_vsync(&mut swapchain, vsync);
        }
        let route = viewport.route(state, rest, true, false, false, input_settings(&world));
        apply_cursor(&window, route.cursor, &mut cursor);
        give_player_input(&mut world, 0, route.world);
        dev.begin_frame(
            commands.stats,
            interval,
            &device,
            engine.assets.get(&library),
            swapchain.format(),
            &jobs,
        );

        // Simulate the next frame on a job while this thread renders the last one.
        let mut next = None;
        let mut presented: Result<Option<PresentStatus>, RunError> = Ok(None);
        let engine_ref = &mut engine;
        let world_ref = &mut world;
        let jobs_ref = &jobs;
        let dev_ref = &mut dev;
        jobs.scope(|s| {
            s.spawn(|| {
                profile_scope!("Simulate");
                engine_ref.frame(world_ref, dt, Some(jobs_ref));
                next = marlin_renderer::take_delta(world_ref);
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
                    dev_ref,
                    &window,
                    vsync,
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
                world.ecs().entity_count(),
                render.mesh_count()
            );
        } else if reported == 0 && engine.is_ready(&world) {
            reported = 1;
        }
        marlin_profile::frame_mark();
        dev.end_frame(now, &window, title);
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
    dev.forget(&mut frames);
    marlin_profile::set_gpu_timings(None);
    Ok(())
}

/// Makes the frame context forget the swapchain's images, before they are recreated or dropped.
fn forget_images(frames: &mut FrameContext, swapchain: &Swapchain) {
    for i in 0..swapchain.image_count() {
        frames.forget_texture(swapchain.texture(i));
    }
}

#[allow(clippy::too_many_arguments, reason = "one frame's inputs")]
fn render_frame(
    device: &marlin_rhi::Device,
    jobs: &JobSystem,
    render: &mut WorldRenderer,
    swapchain: &mut Swapchain,
    frames: &mut FrameContext,
    delta: &RenderDelta,
    dev: &mut DevTools<'_>,
    window: &Window,
    vsync: bool,
) -> Result<PresentStatus, RunError> {
    profile_scope!("Render");
    {
        profile_scope!("Feedback");
        render.collect_feedback(frames);
    }
    {
        profile_scope!("Apply");
        render.apply(device, delta)?;
        // The quality cvars (0008 §2), which the console may have changed.
        marlin_renderer::settings::RenderSettings::from_cvars().apply(render);
    }
    let image = {
        profile_scope!("Acquire");
        swapchain.acquire(None)?
    };
    let texture = swapchain.texture(image);
    let dev_frame = dev.prepare(device, frames, window, vsync, jobs)?;
    let dev = &*dev;
    // The swapchain image is imported like any texture: the graph remembers it was presented.
    {
        profile_scope!("Execute");
        frames.execute_on(device, Some(jobs), |g, _| {
            let target = g.import_texture("Swapchain", texture);
            let mut target = render.add_passes(g, target);
            target = dev.add_passes(g, &dev_frame, target);
            g.present(target);
        })?;
    }
    profile_scope!("Present");
    Ok(swapchain.present()?)
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
