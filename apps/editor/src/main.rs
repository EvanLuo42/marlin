//! The Marlin editor.
//!
//! `marlin-editor <project directory> [--level <uuid>] [--frames N]`: opens the project, runs
//! its cook service in-process (the engine's content with the project's, cooked on demand and
//! reloaded live), and opens the editor's window, drawn by the UI renderer
//! (docs/design/0012-ui-renderer.md). The window shell is [`shell`]; its viewport shows `--level`
//! through the game's renderer, drawn into a texture the UI shows as an external image (0008
//! §6.1). Panels, widgets and docking come with the editor UI framework. `--frames N` quits after
//! N frames (smoke tests); `--screenshot <png>` saves the last of them.

use marlin_editor::shell;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use marlin_app::{App, Event, Surface, WindowDesc};
use marlin_asset::{AssetServer, Uuid};
use marlin_asset_db::{Options, Project};
use marlin_cookd::{Cookd, CookdOptions, bind};
use marlin_cooker::{engine_cookers, open_ddc};
use marlin_diag::{error, info};
use marlin_framework::{ContentSettings, Engine, mount};
use marlin_jobs::{Config, JobSystem};
use marlin_math::Vec2;
use marlin_render_graph::{FrameContext, TextureDesc};
use marlin_renderer::{RenderAssets, WorldRenderer, load_engine_shaders};
use marlin_rhi::{
    DeviceDesc, Format, Instance, InstanceDesc, PresentStatus, SurfaceHandle, Swapchain,
    SwapchainDesc,
};
use marlin_ui_render::{
    Color, ExternalImageId, UiDrawList, UiRenderer, UiRendererDesc, UiShaders, UiSurface,
    UiSurfaceDesc, load_ui_font,
};
use marlin_world::{Modules, Transform, WorldKind};

// Link the whole runtime and every editor library.
use marlin as _;
use marlin_editor_asset_tools as _;
use marlin_editor_core as _;
use marlin_editor_ui_core as _;
use marlin_editor_ui_theme as _;
use marlin_editor_ui_widgets as _;
use marlin_editor_viewport as _;
use marlin_editor_world_tools as _;

#[global_allocator]
static HEAP: marlin_mem::MarlinHeap = marlin_mem::MarlinHeap::new();

fn main() {
    if let Err(missing) = marlin_rt::hosted::init() {
        eprintln!("marlin-editor: {missing}");
        std::process::exit(1);
    }
    // Panics go through marlin_diag (with a backtrace and a crash dump) instead of std's printer.
    std::panic::set_hook(Box::new(|info| {
        let message = info.payload_as_str().unwrap_or("Box<dyn Any>");
        marlin_rt::hosted::report_panic(&message, info.location());
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(project) = args.first() else { usage() };
    let (mut level, mut frames, mut screenshot) = (None, None, None);
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        let value = rest.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--level" => level = Some(value.parse::<Uuid>().unwrap_or_else(|_| usage())),
            "--frames" => frames = Some(value.parse::<u64>().unwrap_or_else(|_| usage())),
            "--screenshot" => screenshot = Some(value.clone()),
            _ => usage(),
        }
    }
    if let Err(e) = run(project, level, frames, screenshot.as_deref()) {
        error!("{e}");
        std::process::exit(1);
    }
}

/// The project's cook service on its own thread, until dropped.
struct CookService {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl CookService {
    fn start(project: &Project) -> Result<Self, String> {
        let (content, _) = project
            .open_content(&Options::default())
            .map_err(|e| format!("opening the project's content: {e}"))?;
        let ddc = open_ddc(&project.ddc_dir(), None).map_err(|e| format!("the cache: {e}"))?;
        let mut server = bind("127.0.0.1:0".parse().expect("an address"))
            .map_err(|e| format!("the cook service: {e:?}"))?;
        let addr = server.local_addr();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("cookd".into())
            .spawn(move || {
                let mut cookd =
                    Cookd::new(content, engine_cookers(), ddc, &CookdOptions::default());
                if let Err(e) = cookd.serve(&mut server, &flag) {
                    error!("cook service: {e:?}");
                }
            })
            .map_err(|e| format!("the cook service's thread: {e}"))?;
        Ok(Self {
            addr,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for CookService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: marlin-editor <project directory> [--level <uuid>] [--frames N [--screenshot <png>]]"
    );
    std::process::exit(2);
}

/// The external image the viewport's scene is drawn as.
const VIEWPORT: ExternalImageId = ExternalImageId(1);
/// The viewport's scene: display-referred, sRGB, as the post chain writes a window.
const VIEWPORT_FORMAT: Format = Format::Rgba8UnormSrgb;

fn run(
    project_dir: &str,
    level: Option<Uuid>,
    max_frames: Option<u64>,
    screenshot: Option<&str>,
) -> Result<(), String> {
    let project = Project::open(project_dir).map_err(|e| format!("{project_dir}: {e}"))?;
    let cookd = CookService::start(&project)?;
    let boot = mount(&ContentSettings {
        packages: Vec::new(),
        cookd: Some(cookd.addr),
        platform: marlin_rt::PLATFORM.into(),
        level: level.unwrap_or_default(),
    })
    .map_err(|e| format!("mounting the cook service: {e}"))?;
    let jobs = JobSystem::new(Config::default()).map_err(|e| format!("jobs: {e:?}"))?;

    // The engine: the game's modules (the editor's join them with the editor UI framework), their
    // assets cooked from content/engine on demand.
    info!("cooking the engine's shaders and UI font");
    let modules = Modules::new(marlin::game_modules()).map_err(|e| format!("{e}"))?;
    let mut engine =
        Engine::new(&boot.vfs, modules, Some(&jobs)).map_err(|e| format!("the engine: {e}"))?;
    // The UI's shaders and font, from an asset server of the editor's own: the engine's is the
    // world's each frame.
    let mut assets = AssetServer::new(engine.loaders());
    let library = load_engine_shaders(&mut assets, &boot.vfs, Some(&jobs))
        .map_err(|e| format!("engine shaders: {e}"))?;
    let font = load_ui_font(&mut assets, &boot.vfs, Some(&jobs))
        .map_err(|e| format!("the UI font: {e:?}"))?;
    let library = assets.get(&library).ok_or("engine shaders: not loaded")?;
    let font = assets.get(&font).ok_or("the UI font: not loaded")?.view();

    let mut app = App::new().map_err(|e| format!("{e}"))?;
    let window = app
        .create_window(&WindowDesc {
            title: "Marlin Editor",
            width: 1600,
            height: 900,
            ..WindowDesc::default()
        })
        .map_err(|e| format!("{e}"))?;
    let mut input = marlin_input::Input::new(&app).map_err(|e| format!("{e}"))?;
    let instance = Instance::new(&InstanceDesc::default()).map_err(|e| format!("{e}"))?;
    let device = instance
        .create_device(&DeviceDesc::default())
        .map_err(|e| format!("{e}"))?;
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
    }
    .map_err(|e| format!("{e}"))?;
    let mut frames = FrameContext::new(&device, &Default::default()).map_err(|e| format!("{e}"))?;
    let shaders = UiShaders::bind(library).map_err(|e| format!("UI shaders: {e}"))?;
    let mut ui = UiRenderer::new(
        &device,
        &shaders,
        &UiRendererDesc {
            output_format: Some(swapchain.format()),
            ..UiRendererDesc::default()
        },
    )
    .map_err(|e| format!("the UI renderer: {e}"))?;
    let window_ui = UiSurface::new(&device, &UiSurfaceDesc::new(1))
        .map_err(|e| format!("the window's UI surface: {e}"))?;

    // The viewport's scene: a world drawn by the game's renderer, into a texture the UI draws
    // (0008 §6.1).
    let mut world = engine.create_world(WorldKind::Game, Some(&jobs));
    let mut scene = match level {
        Some(level) => {
            world.load_sublevel(level, Transform::IDENTITY);
            let render_assets = engine
                .resources()
                .get::<RenderAssets>()
                .ok_or("the engine has no render module")?;
            let ids = marlin_renderer::scene_ids(&world).ok_or("the world has no render scene")?;
            Some(
                WorldRenderer::new(&device, &engine.assets, render_assets, VIEWPORT_FORMAT, ids)
                    .map_err(|e| format!("the scene renderer: {e}"))?,
            )
        }
        None => None,
    };
    let mut viewport_version = 0u64;

    let mut minimized = false;
    // The last frame drew nothing new (§5.5): wait for events instead of spinning.
    let mut idle = false;
    let mut frame = 0u64;
    let mut status = String::from("Ready");
    let (mut fps_start, mut fps_frames) = (Instant::now(), 0u32);
    let mut last = Instant::now();
    'main: loop {
        let timeout = if minimized {
            Duration::from_millis(100)
        } else if idle {
            Duration::from_millis(16)
        } else {
            Duration::ZERO
        };
        for &event in app.pump_timeout(Some(timeout)) {
            match event {
                Event::CloseRequested(_) | Event::Quit => break 'main,
                Event::Resized { size, .. } => {
                    frames.wait_idle().map_err(|e| format!("{e}"))?;
                    forget_images(&mut frames, &swapchain);
                    swapchain
                        .resize(size.width, size.height)
                        .map_err(|e| format!("{e}"))?;
                    window_ui.invalidate();
                }
                Event::Minimized(_) => minimized = true,
                Event::Restored(_) => minimized = false,
                _ => {}
            }
        }
        input.update();
        if minimized {
            continue;
        }
        if max_frames.is_some_and(|n| frame >= n) {
            break;
        }
        frame += 1;
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f32().min(0.1);
        last = now;
        let scale = window.scale_factor();
        let size = window.size();
        let logical = Vec2::new(size.width as f32 / scale, size.height as f32 / scale);
        let cursor = input
            .state()
            .mouse()
            .position()
            .map(|[x, y]| Vec2::new(x / scale, y / scale));
        let view = shell::layout(logical).viewport;
        let (vw, vh) = (
            ((view.width() * scale) as u32).max(1),
            ((view.height() * scale) as u32).max(1),
        );

        // The scene: its fly camera follows the input while the cursor is over the viewport.
        if let Some(scene) = &mut scene {
            let over = cursor.is_some_and(|c| {
                c.x >= view.min.x && c.x < view.max.x && c.y >= view.min.y && c.y < view.max.y
            });
            engine.frame(&mut world, dt, over.then(|| input.state()), Some(&jobs));
            scene.collect_feedback(&mut frames);
            if let Some(delta) = marlin_renderer::take_delta(&mut world) {
                scene.apply(&device, &delta).map_err(|e| format!("{e}"))?;
            }
            viewport_version += 1;
        }

        let fonts = [font];
        let mut list = UiDrawList::new(scale);
        shell::draw(
            &mut list.painter(&ui.glyph_cache().ids).with_fonts(&fonts),
            font,
            &shell::ShellState {
                size: logical,
                cursor,
                status: &status,
                viewport: scene.is_some().then_some(VIEWPORT),
            },
        );
        let externals = [(VIEWPORT, viewport_version)];
        let prepared = ui.prepare(&[&list], &externals, &[font], Some(&jobs));
        let data = &prepared.surfaces[0];
        idle = !window_ui.needs_redraw(data, size.width, size.height, Color::BLACK);
        let last_frame = max_frames == Some(frame);
        if last_frame && screenshot.is_some() {
            window_ui.invalidate();
            idle = false;
        }
        if !idle {
            let image = swapchain.acquire(None).map_err(|e| format!("{e}"))?;
            let texture = swapchain.texture(image);
            let shot = frames
                .execute_on(&device, Some(&jobs), |g, _| {
                    let target = g.import_texture("Swapchain", texture);
                    let viewport = scene.as_mut().map(|scene| {
                        let t = g.create_texture(
                            "Viewport",
                            TextureDesc::new_2d(vw, vh, VIEWPORT_FORMAT),
                        );
                        (VIEWPORT, scene.add_passes(g, t))
                    });
                    let pages = ui.begin(g, &prepared);
                    let out = ui.add_passes(
                        g,
                        pages,
                        &window_ui,
                        data,
                        size.width,
                        size.height,
                        Color::BLACK,
                        viewport.as_slice(),
                    );
                    let shot = (last_frame && screenshot.is_some())
                        .then(|| g.readback_texture(out.target));
                    let target = ui.encode(g, out.target, target);
                    g.present(target);
                    shot
                })
                .map_err(|e| format!("{e}"))?;
            if swapchain.present().map_err(|e| format!("{e}"))? == PresentStatus::Occluded {
                app.pump_timeout(Some(Duration::from_millis(16)));
            }
            fps_frames += 1;
            if let (Some(ticket), Some(path)) = (shot, screenshot) {
                let pixels = frames.wait_readback(ticket).map_err(|e| format!("{e}"))?;
                save_png(path, size.width, size.height, &pixels)?;
                info!("screenshot: {path}");
            }
        }
        let elapsed = fps_start.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let fps = fps_frames as f32 / elapsed.as_secs_f32();
            let occupancy = ui.glyph_cache().page_occupancy();
            status = format!(
                "Ready  ·  {fps:.0} fps  ·  {} commands  ·  glyph cache {:.1}% of {} page(s)  ·  {}",
                data.commands().len(),
                occupancy.iter().sum::<f32>() * 100.0 / occupancy.len().max(1) as f32,
                occupancy.len(),
                project.name,
            );
            (fps_start, fps_frames) = (Instant::now(), 0);
        }
    }
    frames.wait_idle().map_err(|e| format!("{e}"))?;
    forget_images(&mut frames, &swapchain);
    ui.forget(&mut frames);
    window_ui.forget(&mut frames);
    drop(scene);
    drop(cookd);
    Ok(())
}

/// Makes the frame context forget the swapchain's images, before they are recreated or dropped.
fn forget_images(frames: &mut FrameContext, swapchain: &Swapchain) {
    for i in 0..swapchain.image_count() {
        frames.forget_texture(swapchain.texture(i));
    }
}

/// Writes RGBA8 rows to a PNG at `path`.
fn save_png(path: &str, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| format!("{path}: {e}"))?;
    writer
        .write_image_data(rgba)
        .map_err(|e| format!("{path}: {e}"))
}
