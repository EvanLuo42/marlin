//! The host loop (0019 §12, §13.1): the platform's windows, one device, the engine, and an
//! application ([`HostApp`]: the game or the editor) whose frames it pipelines.
//!
//! Each turn of the loop renders the previous frame of the application on this thread while a
//! job simulates the next one, so the CPU sides overlap (Game N+1 ∥ Render N, with the GPU on
//! N-1 through two frames in flight). Before the job starts, the host reads the windows' events
//! and the input, runs its own commands (§9.2) and hands the rest to the application.
//!
//! **Windows** (0008 §6.4): the host owns them, each with its own swapchain; one frame graph
//! imports and presents them all. The application sees them as [`WindowState`]s, asks for new
//! ones and closes them with [`WindowRequest`]s (opened after the frame), and draws into each
//! one's image ([`WindowTarget`]). The first window is the main one: closing it quits, the
//! development overlay draws over it. A game uses only it; an editor opens more (floating
//! panels, a second scene view).

use alloc::string::String;
use alloc::vec::Vec;

use core::time::Duration;

use marlin_app::{App, Event, Size, Surface, Window, WindowDesc, WindowId};
use marlin_diag::info;
use marlin_input::{Input, InputState};
use marlin_input_action::{CursorMode, InputFrame};
use marlin_jobs::{Config, JobSystem};
use marlin_profile::profile_scope;
use marlin_render_graph::{
    FrameContext, FrameContextDesc, GpuTimeline, RenderGraph, TextureHandle,
};
use marlin_renderer::{RenderShaders, WorldRendererError};
use marlin_rhi::{
    Device, DeviceDesc, Format, Instance, InstanceDesc, PresentMode, PresentStatus, SurfaceHandle,
    Swapchain, SwapchainDesc,
};
use marlin_time::Instant;
use marlin_world::Modules;

use crate::boot::{Boot, R_VSYNC};
use crate::dev::DevTools;
use crate::engine::{Engine, EngineError};
use crate::input::{HostInput, apply_cursor, player_zero};

#[derive(Debug, marlin_error::Error)]
pub enum RunError {
    #[error("platform")]
    Platform(#[from] marlin_app::Error),
    #[error("gpu")]
    Gpu(#[from] marlin_rhi::Error),
    #[error("engine")]
    Engine(#[from] EngineError),
    #[error("a world renderer")]
    Render(#[from] WorldRendererError),
    #[error("no render module: this build cannot draw")]
    NoRenderer,
    #[error("{0}")]
    App(String),
}

/// A window as the application sees it each frame.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct WindowState {
    pub id: WindowId,
    /// The client area, in physical pixels.
    pub size: Size,
    pub scale: f32,
    /// The first window: closing it quits.
    pub main: bool,
    pub focused: bool,
    pub minimized: bool,
    /// What the cursor does over it; the application sets it, the host applies it after the
    /// frame.
    pub cursor: CursorMode,
}

/// A change to the window set, made after the frame that asks for it.
#[derive(Clone, PartialEq, Debug)]
pub enum WindowRequest {
    Open {
        title: String,
        width: u32,
        height: u32,
    },
    /// Closes a window other than the main one.
    Close(WindowId),
}

/// A window's image in a frame's graph: the application draws into `image` and leaves the
/// handle it ends with there; the host presents it.
pub struct WindowTarget<'g> {
    pub id: WindowId,
    pub size: Size,
    pub main: bool,
    pub image: TextureHandle<'g>,
}

/// What the application starts with.
pub struct HostSetup<'a, 'v> {
    pub engine: &'a mut Engine<'v>,
    pub boot: &'a Boot,
    pub device: &'a Device,
    pub jobs: &'a JobSystem,
    /// The windows' image format: what the application's last pass writes.
    pub output: Format,
    /// The main window.
    pub window: WindowState,
}

impl HostSetup<'_, '_> {
    /// The render module's shaders, for the application's world renderers.
    pub fn render_shaders(&self) -> Result<RenderShaders, RunError> {
        self.engine
            .resources()
            .get::<RenderShaders>()
            .cloned()
            .ok_or(RunError::NoRenderer)
    }
}

/// One frame of the application's simulation side.
pub struct SimContext<'a, 'v> {
    pub engine: &'a mut Engine<'v>,
    pub jobs: &'a JobSystem,
    /// Simulated time: the wall-clock interval, clamped to 0.1 s.
    pub dt: f32,
    /// The wall-clock interval since the last frame.
    pub interval: f32,
    /// The platform's input state: pointer position (and which window it is over), buttons.
    pub state: InputState,
    /// The keyboard, mouse and first gamepad, without what the host's commands used.
    pub input: InputFrame,
    /// The open windows, the main one first. Set each one's `cursor`.
    pub windows: Vec<WindowState>,
    /// Windows the user closed since the last frame (the main one never: that quits).
    pub closed: Vec<WindowId>,
    /// Windows to open or close after this frame.
    pub requests: Vec<WindowRequest>,
}

impl SimContext<'_, '_> {
    pub fn main_window(&self) -> &WindowState {
        &self.windows[0]
    }

    pub fn window_mut(&mut self, id: WindowId) -> Option<&mut WindowState> {
        self.windows.iter_mut().find(|w| w.id == id)
    }
}

/// The render side's context.
pub struct RenderContext<'a> {
    pub device: &'a Device,
    pub frames: &'a mut FrameContext,
    pub jobs: &'a JobSystem,
    pub output: Format,
}

/// An application a host runs (0019 §13.1): its simulation side runs on a job, its render side
/// on the main thread, a frame apart.
pub trait HostApp {
    /// The game side: worlds, the editor's documents, UI recording.
    type Sim: Send;
    /// One frame for the render side: deltas, views, UI draw lists.
    type Frame: Send;
    /// The render side: world renderers, the windows' UI renderers.
    type Render;

    /// One frame of the simulation side.
    fn simulate(sim: &mut Self::Sim, cx: &mut SimContext<'_, '_>) -> Self::Frame;

    /// Takes `frame` in before its passes are declared: streaming feedback, deltas applied,
    /// world renderers made for new worlds.
    fn prepare(
        render: &mut Self::Render,
        cx: &mut RenderContext<'_>,
        frame: &Self::Frame,
    ) -> Result<(), RunError>;

    /// Declares `frame` into `g`, drawing into the images of `windows` (those not minimized).
    fn render<'g>(
        render: &'g mut Self::Render,
        frame: &'g Self::Frame,
        g: &mut RenderGraph<'g>,
        windows: &mut [WindowTarget<'g>],
    );

    /// The GPU is idle and the loop is over: forget the resources the frame context tracks.
    fn shutdown(_render: &mut Self::Render, _frames: &mut FrameContext) {}
}

/// How a host runs.
#[derive(Clone, Debug)]
pub struct HostOptions<'a> {
    /// The main window's.
    pub title: &'a str,
    pub width: u32,
    pub height: u32,
    /// Quit after this many frames (smoke tests).
    pub max_frames: Option<u64>,
    /// Read back the last frame's main window image (screenshots).
    pub capture_last_frame: bool,
}

/// How the loop ended.
#[derive(Default)]
pub struct HostExit {
    /// The last frame's main window image, RGBA8 rows, if asked for: width, height, bytes.
    pub last_frame: Option<(u32, u32, Vec<u8>)>,
}

/// What a turn of the loop asks of its caller.
enum Step {
    Continue,
    Quit,
    /// Every window is hidden behind others: idle a moment before the next turn.
    Occluded,
}

/// A window and its swapchain.
struct HostWindow {
    // Declared first: dropped before the window it presents to.
    swapchain: Swapchain,
    window: Window,
    minimized: bool,
    focused: bool,
    cursor: Option<CursorMode>,
    /// The cursor the application asked for last.
    wanted: CursorMode,
}

impl HostWindow {
    fn open(
        app: &App,
        device: &Device,
        desc: &WindowDesc<'_>,
        vsync: bool,
    ) -> Result<Self, RunError> {
        let window = app.create_window(desc)?;
        let Surface::Win32 { hwnd, hinstance } = window.surface() else {
            unreachable!("only Win32 windows exist on this platform")
        };
        let size = window.size();
        // SAFETY: the swapchain is dropped before the window (field order).
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
        set_vsync(&mut swapchain, vsync);
        Ok(Self {
            swapchain,
            window,
            minimized: false,
            focused: true,
            cursor: None,
            wanted: CursorMode::Visible,
        })
    }

    fn state(&self, main: bool) -> WindowState {
        WindowState {
            id: self.window.id(),
            size: self.window.size(),
            scale: self.window.scale_factor(),
            main,
            focused: self.focused,
            minimized: self.minimized,
            cursor: self.wanted,
        }
    }

    /// Makes `frames` forget the swapchain's images, before they are recreated or dropped.
    fn forget_images(&self, frames: &mut FrameContext) {
        for i in 0..self.swapchain.image_count() {
            frames.forget_texture(self.swapchain.texture(i));
        }
    }
}

/// Runs the application `A` over the engine of `modules` (registered before `boot` read its
/// configuration), until its main window is closed, the host's quit command fires or
/// `options.max_frames` have run. `setup` makes the application's two sides.
pub fn run_app<A: HostApp>(
    boot: &Boot,
    modules: Modules,
    options: &HostOptions<'_>,
    setup: impl FnOnce(&mut HostSetup<'_, '_>) -> Result<(A::Sim, A::Render), RunError>,
) -> Result<HostExit, RunError> {
    let mut app = App::new()?;
    let input = Input::new(&app)?;
    let jobs = JobSystem::new(Config::default())?;
    let instance = Instance::new(&InstanceDesc::default())?;
    let device = instance.create_device(&DeviceDesc::default())?;
    info!("adapter: {}", device.adapter_info().name());

    let vsync = R_VSYNC.get();
    let main = HostWindow::open(
        &app,
        &device,
        &WindowDesc {
            title: options.title,
            width: options.width,
            height: options.height,
            ..WindowDesc::default()
        },
        vsync,
    )?;
    let output = main.swapchain.format();
    // The passes' GPU times, for the profiler.
    static TIMELINE: GpuTimeline = GpuTimeline::new();
    marlin_profile::set_gpu_timings(Some(&TIMELINE));
    let frames = FrameContext::new(
        &device,
        &FrameContextDesc {
            timeline: Some(&TIMELINE),
            // Independent passes run side by side (0007 §4.8).
            overlap: true,
            ..FrameContextDesc::default()
        },
    )?;

    let mut engine = Engine::new(&boot.vfs, modules, Some(&jobs))?;
    let (sim, render) = setup(&mut HostSetup {
        engine: &mut engine,
        boot,
        device: &device,
        jobs: &jobs,
        output,
        window: main.state(true),
    })?;
    // The host's commands (0019 §9.2), and its development tools when the build has them.
    let host_input = HostInput::load(&mut engine.assets, &boot.vfs, Some(&jobs));
    let dev = DevTools::new(&engine, device.adapter_info().name());

    let mut host = HostLoop::<A> {
        device: &device,
        jobs: &jobs,
        options,
        input,
        engine,
        host_input,
        dev,
        sim,
        render,
        windows: alloc::vec![main],
        frames,
        output,
        vsync,
        pending: None,
        closed: Vec::new(),
        opening: Vec::new(),
        last: Instant::now(),
        frame_count: 0,
        exit: HostExit::default(),
    };
    loop {
        let idle = host.windows.iter().all(|w| w.minimized);
        let timeout = Some(if idle {
            Duration::from_millis(100)
        } else {
            Duration::ZERO
        });
        // The loop turns from the pump's tick while the OS holds it (a window drag), then after
        // it on the events gathered since the last tick (the drag's final size among them).
        let mut step = Ok(Step::Continue);
        let events: Vec<Event> = app
            .pump_with(timeout, |events| {
                if matches!(step, Ok(Step::Continue)) {
                    step = host.turn(events);
                }
            })
            .to_vec();
        let step = match step? {
            Step::Quit => break,
            _ => host.turn(&events)?,
        };
        // Windows the application asked for open between turns: opening needs the app.
        host.open_windows(&app)?;
        match step {
            Step::Quit => break,
            Step::Occluded => {
                app.pump_timeout(Some(Duration::from_millis(16)));
            }
            Step::Continue => {}
        }
    }
    host.finish()
}

/// The host's state between turns of the loop.
struct HostLoop<'h, 'v, A: HostApp> {
    device: &'h Device,
    jobs: &'h JobSystem,
    options: &'h HostOptions<'h>,
    input: Input,
    engine: Engine<'v>,
    host_input: HostInput,
    dev: DevTools,
    sim: A::Sim,
    render: A::Render,
    /// The main window first.
    windows: Vec<HostWindow>,
    frames: FrameContext,
    output: Format,
    vsync: bool,
    /// The last frame simulated, rendered by the next turn.
    pending: Option<A::Frame>,
    /// Windows closed since the application last saw the set.
    closed: Vec<WindowId>,
    /// Windows the application asked for, opened after the turn.
    opening: Vec<(String, u32, u32)>,
    last: Instant,
    frame_count: u64,
    exit: HostExit,
}

impl<A: HostApp> HostLoop<'_, '_, A> {
    fn find(&self, id: WindowId) -> Option<usize> {
        self.windows.iter().position(|w| w.window.id() == id)
    }

    /// Closes window `i` (not the main one): the graph forgets its images first.
    fn close(&mut self, i: usize) -> Result<(), RunError> {
        self.frames.wait_idle()?;
        self.windows[i].forget_images(&mut self.frames);
        let w = self.windows.remove(i);
        self.closed.push(w.window.id());
        Ok(())
    }

    /// One turn: the events since the last, then simulate the next frame and render the last.
    fn turn(&mut self, events: &[Event]) -> Result<Step, RunError> {
        for &event in events {
            match event {
                Event::Quit => return Ok(Step::Quit),
                Event::CloseRequested(id) => match self.find(id) {
                    Some(0) => return Ok(Step::Quit),
                    Some(i) => self.close(i)?,
                    None => {}
                },
                Event::Resized { window, size } => {
                    if let Some(i) = self.find(window) {
                        // The images go: the graph forgets them first.
                        self.frames.wait_idle()?;
                        self.windows[i].forget_images(&mut self.frames);
                        self.windows[i].swapchain.resize(size.width, size.height)?;
                    }
                }
                Event::Minimized(id) | Event::Restored(id) => {
                    if let Some(i) = self.find(id) {
                        self.windows[i].minimized = matches!(event, Event::Minimized(_));
                    }
                }
                Event::Focused { window, focused } => {
                    if let Some(i) = self.find(window) {
                        self.windows[i].focused = focused;
                    }
                }
                _ => {}
            }
        }
        if self
            .options
            .max_frames
            .is_some_and(|n| self.frame_count >= n)
        {
            return Ok(Step::Quit);
        }
        self.frame_count += 1;
        let capture =
            self.options.capture_last_frame && self.options.max_frames == Some(self.frame_count);
        self.input.update();
        let state = *self.input.state();
        let now = Instant::now();
        let interval = now.duration_since(self.last).as_secs_f32();
        let dt = interval.min(0.1);
        self.last = now;

        let (commands, rest) = self.host_input.route(player_zero(&state), interval);
        if commands.quit {
            return Ok(Step::Quit);
        }
        if commands.fullscreen
            && let Some(w) = self.windows.iter().find(|w| w.focused)
        {
            w.window.toggle_fullscreen()?;
        }
        if commands.vsync {
            self.vsync = !self.vsync;
            for w in &mut self.windows {
                set_vsync(&mut w.swapchain, self.vsync);
            }
        }
        self.dev
            .begin_frame(commands.stats, interval, self.device, self.output);

        // Simulate the next frame on a job while this thread renders the last one.
        let Self {
            device,
            jobs,
            engine,
            dev,
            sim,
            render,
            windows,
            frames,
            vsync,
            pending,
            closed,
            ..
        } = self;
        let mut cx = SimContext {
            engine,
            jobs,
            dt,
            interval,
            state,
            input: rest,
            windows: windows
                .iter()
                .enumerate()
                .map(|(i, w)| w.state(i == 0))
                .collect(),
            closed: core::mem::take(closed),
            requests: Vec::new(),
        };
        let mut next = None;
        let mut presented: Result<Option<Presented>, RunError> = Ok(None);
        let visible = windows.iter().any(|w| !w.minimized);
        let cx_ref = &mut cx;
        jobs.scope(|s| {
            s.spawn(|| {
                profile_scope!("Simulate");
                next = Some(A::simulate(sim, cx_ref));
            });
            if let Some(frame) = pending.take()
                && visible
            {
                presented = render_frame::<A>(
                    device, jobs, render, windows, frames, &frame, dev, *vsync, capture,
                )
                .map(Some);
            }
        });
        let presented = presented?;
        *pending = next;

        // The application's window changes: cursors now, opening and closing for the next frame.
        let SimContext {
            windows: states,
            requests,
            ..
        } = cx;
        for state in &states {
            if let Some(w) = self.windows.iter_mut().find(|w| w.window.id() == state.id) {
                w.wanted = state.cursor;
                apply_cursor(&w.window, state.cursor, &mut w.cursor);
            }
        }
        for request in requests {
            match request {
                WindowRequest::Open {
                    title,
                    width,
                    height,
                } => self.opening.push((title, width, height)),
                WindowRequest::Close(id) => {
                    if let Some(i) = self.find(id).filter(|&i| i > 0) {
                        self.close(i)?;
                    }
                }
            }
        }
        if let Some(Presented {
            main_image: Some(image),
            ..
        }) = &presented
        {
            self.exit.last_frame = Some(image.clone());
        }
        marlin_profile::frame_mark();
        self.dev
            .end_frame(now, &self.windows[0].window, self.options.title);
        Ok(match presented {
            Some(p) if p.all_occluded => Step::Occluded,
            _ => Step::Continue,
        })
    }

    /// Opens the windows the application asked for.
    fn open_windows(&mut self, app: &App) -> Result<(), RunError> {
        for (title, width, height) in core::mem::take(&mut self.opening) {
            let desc = WindowDesc {
                title: &title,
                width,
                height,
                ..WindowDesc::default()
            };
            self.windows
                .push(HostWindow::open(app, self.device, &desc, self.vsync)?);
        }
        Ok(())
    }

    /// The loop is over: the GPU idle, every resource forgotten.
    fn finish(mut self) -> Result<HostExit, RunError> {
        self.frames.wait_idle()?;
        for w in &self.windows {
            w.forget_images(&mut self.frames);
        }
        self.dev.forget(&mut self.frames);
        A::shutdown(&mut self.render, &mut self.frames);
        marlin_profile::set_gpu_timings(None);
        Ok(self.exit)
    }
}

/// What presenting a frame gave.
struct Presented {
    /// Every window presented was hidden behind others.
    all_occluded: bool,
    main_image: Option<(u32, u32, Vec<u8>)>,
}

#[allow(clippy::too_many_arguments, reason = "one frame's inputs")]
fn render_frame<A: HostApp>(
    device: &Device,
    jobs: &JobSystem,
    render: &mut A::Render,
    windows: &mut [HostWindow],
    frames: &mut FrameContext,
    frame: &A::Frame,
    dev: &mut DevTools,
    vsync: bool,
    capture: bool,
) -> Result<Presented, RunError> {
    profile_scope!("Render");
    {
        profile_scope!("Prepare");
        A::prepare(
            render,
            &mut RenderContext {
                device,
                frames,
                jobs,
                output: windows[0].swapchain.format(),
            },
            frame,
        )?;
    }
    // Each visible window's image for this frame.
    let mut images = Vec::new();
    {
        profile_scope!("Acquire");
        for (i, w) in windows.iter_mut().enumerate() {
            if !w.minimized {
                images.push((i, w.swapchain.acquire(None)?));
            }
        }
    }
    let main_visible = images.first().is_some_and(|&(i, _)| i == 0);
    let dev_frame = if main_visible {
        Some(dev.prepare(device, frames, &windows[0].window, vsync, jobs)?)
    } else {
        None
    };
    let dev = &*dev;
    let main_size = windows[0].window.size();
    let shot = {
        profile_scope!("Execute");
        let windows = &*windows;
        frames.execute_on(device, Some(jobs), |g, _| {
            // The swapchain images are imported like any texture: the graph remembers they were
            // presented.
            let mut targets: Vec<WindowTarget<'_>> = images
                .iter()
                .map(|&(i, image)| WindowTarget {
                    id: windows[i].window.id(),
                    size: windows[i].window.size(),
                    main: i == 0,
                    image: g.import_texture("Swapchain", windows[i].swapchain.texture(image)),
                })
                .collect();
            A::render(render, frame, g, &mut targets);
            let mut shot = None;
            for t in &mut targets {
                if t.main {
                    if let Some(dev_frame) = &dev_frame {
                        t.image = dev.add_passes(g, dev_frame, t.image);
                    }
                    shot = capture.then(|| g.readback_texture(t.image));
                }
                g.present(t.image);
            }
            shot
        })?
    };
    let main_image = match shot {
        Some(ticket) => Some((
            main_size.width,
            main_size.height,
            frames.wait_readback(ticket)?,
        )),
        None => None,
    };
    profile_scope!("Present");
    let mut all_occluded = true;
    for &(i, _) in &images {
        if windows[i].swapchain.present()? != PresentStatus::Occluded {
            all_occluded = false;
        }
    }
    Ok(Presented {
        all_occluded,
        main_image,
    })
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
