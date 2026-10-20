//! The Marlin editor.
//!
//! `marlin-editor <project> [--level <uuid>] [--frames N]`: opens the project (its directory or
//! `.mproject` file), applies the engine's and the project's `config/` from source
//! (`editor.toml` included; 0003 §4.6), runs its cook service in-process (the engine's content
//! with the project's, cooked on demand and reloaded live), and opens the editor's window, drawn
//! by the UI renderer (docs/design/0012-ui-renderer.md), on the engine's host like the game
//! (0019 §13): the editor is [`EditorApp`]. Its viewport shows `--level`, else
//! `editor.startup_level`, else `game.default_level`, read from the project's sources, through
//! its own camera; Alt+P plays it in editor and Shift+F1 frees the mouse while playing. Panels,
//! widgets and docking come with the editor UI framework. `--frames N` quits after N frames
//! (smoke tests); `--screenshot <png>` saves the last of them.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use marlin_asset::Uuid;
use marlin_asset_db::{ConfigScope, Options, Project};
use marlin_config::CvarString;
use marlin_cookd::{Cookd, CookdOptions, bind};
use marlin_cooker::{engine_cookers, open_ddc};
use marlin_diag::{error, info, warn};
use marlin_editor::app::{EditorApp, EditorStart};
use marlin_editor_core::SourceContent;
use marlin_framework::boot::GAME_DEFAULT_LEVEL;
use marlin_framework::{
    ContentSettings, HostOptions, apply_config_files, level_ref, mount, run_app,
};
use marlin_world::Modules;

// Link the whole runtime and every editor library.
use marlin as _;
use marlin_editor_asset_tools as _;
use marlin_editor_ui_core as _;
use marlin_editor_ui_theme as _;
use marlin_editor_ui_widgets as _;
use marlin_editor_world_tools as _;

/// The level the editor opens (UE's EditorStartupMap), in the project's `config/editor.toml`.
static EDITOR_STARTUP_LEVEL: CvarString = CvarString::new(
    "editor.startup_level",
    "",
    "The level the editor opens: a link (</Game/levels/main:Main #<uuid>>) or a UUID; \
     game.default_level when empty.",
);

/// `editor.startup_level`, else `game.default_level`; `None` when neither is set.
fn startup_level() -> Result<Option<Uuid>, String> {
    for cvar in [&EDITOR_STARTUP_LEVEL, &GAME_DEFAULT_LEVEL] {
        let text = cvar.get();
        if !text.trim().is_empty() {
            return level_ref(&text)
                .map(Some)
                .ok_or_else(|| format!("{}: not a link or a UUID: {text:?}", cvar.name()));
        }
    }
    Ok(None)
}

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
    eprintln!("usage: marlin-editor <project> [--level <uuid>] [--frames N [--screenshot <png>]]");
    std::process::exit(2);
}

fn run(
    project_dir: &str,
    level: Option<Uuid>,
    max_frames: Option<u64>,
    screenshot: Option<&str>,
) -> Result<(), String> {
    let project = Project::open(project_dir).map_err(|e| format!("{project_dir}: {e}"))?;
    // The game's modules (the editor's join them with the editor UI framework), registered
    // before the config applies so their cvars take its values.
    let modules = Modules::new(marlin::game_modules()).map_err(|e| format!("{e}"))?;
    marlin_framework::register_cvars();
    let _ = marlin_config::register(&EDITOR_STARTUP_LEVEL);
    let config = project
        .config_files(marlin_rt::PLATFORM, ConfigScope::Editor)
        .map_err(|e| format!("{e}"))?;
    for warning in apply_config_files(&config) {
        warn!("config: {warning}");
    }
    let level = match level {
        Some(level) => Some(level),
        None => startup_level()?,
    };

    let cookd = CookService::start(&project)?;
    let boot = mount(&ContentSettings {
        packages: Vec::new(),
        cookd: Some(cookd.addr),
        platform: marlin_rt::PLATFORM.into(),
        level: level.unwrap_or_default(),
    })
    .map_err(|e| format!("mounting the cook service: {e}"))?;
    // The level is read from the project's sources; the engine's assets come cooked on demand.
    let sources = SourceContent::open(&project, engine_cookers())?;

    let options = HostOptions {
        title: "Marlin Editor",
        width: 1600,
        height: 900,
        max_frames,
        capture_last_frame: screenshot.is_some(),
    };
    let start = EditorStart {
        sources,
        project: project.name.clone(),
        level,
    };
    let exit = run_app::<EditorApp>(&boot, modules, &options, |setup| {
        EditorApp::setup(setup, start)
    })
    .map_err(|e| format!("{e}"))?;
    if let (Some((width, height, pixels)), Some(path)) = (exit.last_frame, screenshot) {
        save_png(path, width, height, &pixels)?;
        info!("screenshot: {path}");
    }
    drop(cookd);
    Ok(())
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
