//! The whole pipeline on real hardware: `samples/basic` cooked, packaged, booted, rendered and
//! read back; the same through a live cook service; and an edit hot-reloading into the picture.
#![cfg(marlin_platform = "windows")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use marlin_asset::Uuid;
use marlin_asset_db::{AssetDb, Options};
use marlin_cook_core::{CookOptions, PackageOptions, cook_all, publish_manifest, write_package};
use marlin_cookd::{Cookd, CookdOptions, bind};
use marlin_cooker::{engine_cookers, open_ddc};
use marlin_framework::{
    Boot, ContentSettings, Engine, OFFSCREEN_FORMAT, Offscreen, RenderSide, mount,
};
use marlin_render::{Renderer, RendererDesc};
use marlin_rhi::{AdapterPreference, Device, DeviceDesc, Instance, InstanceDesc};

const LEVEL: &str = "01992d00-0000-7000-8000-000000000001";
const WIDTH: u32 = 160;
const HEIGHT: u32 = 90;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            copy_dir(&path, &to.join(&name));
        } else {
            std::fs::copy(&path, to.join(&name)).unwrap();
        }
    }
}

/// A copy of the sample, so tests can edit it.
fn sample(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("marlin-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let sample = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../samples/basic");
    copy_dir(&sample, &root);
    root
}

/// Cooks and packages `root`; returns the package path.
fn package(root: &Path) -> String {
    let db = AssetDb::build(
        root,
        Options {
            journal: false,
            ..Options::default()
        },
    )
    .unwrap();
    let ddc = open_ddc(root, None).unwrap();
    let (report, manifest) = cook_all(
        &db,
        &engine_cookers(),
        ddc.as_ref(),
        &CookOptions::default(),
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    publish_manifest(root, ddc.as_ref(), &manifest).unwrap();
    let out = root.join("game.mpak");
    write_package(&manifest, ddc.as_ref(), &PackageOptions::default(), &out).unwrap();
    out.to_string_lossy().into_owned()
}

/// The hardware adapter with validation on, shared by every test in this binary and never
/// dropped: tearing down `Device`s while other threads use the same adapter can deadlock the
/// NVIDIA driver (see the RHI tests' `support::devices`).
fn device() -> &'static Device {
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

struct Viewer {
    device: &'static Device,
    render: RenderSide,
    offscreen: Offscreen,
}

impl Viewer {
    fn new() -> Self {
        let device = device();
        let renderer = Renderer::new(
            device,
            &RendererDesc {
                color_format: OFFSCREEN_FORMAT,
                frames_in_flight: 2,
                width: WIDTH,
                height: HEIGHT,
            },
        )
        .unwrap();
        let offscreen = Offscreen::new(device, WIDTH, HEIGHT).unwrap();
        Self {
            render: RenderSide::new(renderer, 2),
            offscreen,
            device,
        }
    }

    /// One frame of `engine` (time standing still, so spinning objects hold their pose).
    fn frame(&mut self, engine: &mut Engine<'_>) -> Vec<u32> {
        let delta = engine.frame(0.0, None, None);
        self.offscreen
            .render(self.device, &mut self.render, &delta)
            .unwrap()
    }
}

/// Runs frames until the level is loaded, then returns the picture.
fn load(boot: &Boot, viewer: &mut Viewer) -> Vec<u32> {
    let mut engine = Engine::new(&boot.vfs).unwrap();
    engine.load_level(boot.level);
    let start = Instant::now();
    while !engine.is_ready() {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the level never loaded: {:?}",
            engine.level_state()
        );
        viewer.frame(&mut engine);
        std::thread::sleep(Duration::from_millis(2));
    }
    // The frame that sent the last uploads, then one more to be sure everything is drawn.
    viewer.frame(&mut engine);
    viewer.frame(&mut engine)
}

fn rgb(p: u32) -> [u8; 3] {
    [p as u8, (p >> 8) as u8, (p >> 16) as u8]
}

fn at(pixels: &[u32], x: u32, y: u32) -> [u8; 3] {
    rgb(pixels[(y * WIDTH + x) as usize])
}

/// What the sample must look like: background at the top corners, the orange sphere in the
/// middle, the grey ground at the bottom.
fn check_sample(pixels: &[u32]) {
    let clear = [5, 8, 13]; // ClearColor (0.02, 0.03, 0.05) in 8-bit linear
    for (x, y) in [(1, 1), (WIDTH - 2, 1)] {
        let p = at(pixels, x, y);
        assert!(
            p.iter().zip(clear).all(|(a, b)| a.abs_diff(b) <= 1),
            "background at ({x}, {y}): {p:?}"
        );
    }
    let center = at(pixels, WIDTH / 2, HEIGHT / 2);
    assert!(
        center[0] > center[1] && center[1] > center[2] && center[0] > 60,
        "the orange sphere: {center:?}"
    );
    let ground = at(pixels, WIDTH / 2, HEIGHT - 3);
    let spread = ground.iter().max().unwrap() - ground.iter().min().unwrap();
    assert!(ground[0] > 20 && spread < 12, "the grey ground: {ground:?}");
    let lit = pixels.iter().filter(|&&p| rgb(p) != [5, 8, 13]).count();
    assert!(
        lit > pixels.len() / 3,
        "the scene covers the view: {lit} pixels"
    );
}

struct RunningCookd {
    addr: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RunningCookd {
    fn start(root: &Path) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let (root, s) = (root.to_owned(), stop.clone());
        let thread = std::thread::spawn(move || {
            let (db, _) = AssetDb::open(&root, Options::default()).unwrap();
            let ddc = open_ddc(&root, None).unwrap();
            let options = CookdOptions {
                workers: 2,
                watch_interval: Duration::from_millis(30),
            };
            let mut cookd = Cookd::new(db, engine_cookers(), ddc, &options);
            let mut server = bind("127.0.0.1:0".parse().unwrap()).unwrap();
            tx.send(server.local_addr()).unwrap();
            cookd.serve(&mut server, &s).unwrap();
        });
        Self {
            addr: rx.recv().unwrap(),
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for RunningCookd {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}

#[test]
fn the_sample_cooks_packages_and_renders_the_same_from_cookd() {
    let root = sample("package");
    let package = package(&root);
    let level: Uuid = LEVEL.parse().unwrap();
    let mut viewer = Viewer::new();

    let boot = mount(&ContentSettings {
        packages: vec![package],
        cookd: None,
        platform: "windows".into(),
        level,
    })
    .unwrap();
    let packaged = load(&boot, &mut viewer);
    check_sample(&packaged);
    assert_eq!(
        viewer.render.mesh_count(),
        3,
        "cube, sphere and torus on the GPU"
    );

    // The same content streamed from a cook service: the same picture, pixel for pixel.
    let cookd = RunningCookd::start(&root);
    let boot = mount(&ContentSettings {
        packages: Vec::new(),
        cookd: Some(cookd.addr),
        platform: "windows".into(),
        level,
    })
    .unwrap();
    let mut viewer2 = Viewer::new();
    let streamed = load(&boot, &mut viewer2);
    assert!(
        streamed == packaged,
        "cookd and the package render the same"
    );
    drop(cookd);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn editing_the_level_changes_the_picture_live() {
    let root = sample("reload");
    let cookd = RunningCookd::start(&root);
    let level: Uuid = LEVEL.parse().unwrap();
    let boot = mount(&ContentSettings {
        packages: Vec::new(),
        cookd: Some(cookd.addr),
        platform: "windows".into(),
        level,
    })
    .unwrap();
    let mut viewer = Viewer::new();
    let mut engine = Engine::new(&boot.vfs).unwrap();
    engine.load_level(boot.level);
    let start = Instant::now();
    while !engine.is_ready() {
        assert!(start.elapsed() < Duration::from_secs(60), "never loaded");
        viewer.frame(&mut engine);
    }
    viewer.frame(&mut engine);
    let before = viewer.frame(&mut engine);
    check_sample(&before);

    // The sphere turns green in the text; the running game follows.
    let path = root.join("levels/main.masset");
    let text = std::fs::read_to_string(&path).unwrap();
    let edited = text.replace(
        "Color = (0.9, 0.55, 0.2, 1.0)",
        "Color = (0.1, 0.9, 0.2, 1.0)",
    );
    assert_ne!(edited, text);
    std::fs::write(&path, edited).unwrap();
    let start = Instant::now();
    loop {
        let pixels = viewer.frame(&mut engine);
        let c = at(&pixels, WIDTH / 2, HEIGHT / 2);
        let [r, g, b] = c.map(u32::from);
        if g > r * 2 && g > b * 2 {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the edit never showed: {c:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // Only the level reloaded; its meshes stayed on the GPU.
    assert_eq!(viewer.render.mesh_count(), 3);
    drop(cookd);
    let _ = std::fs::remove_dir_all(root);
}
