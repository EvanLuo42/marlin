//! The whole pipeline on real hardware: `samples/basic` cooked, packaged, booted, rendered with
//! the engine's cooked shaders (`content/engine`, cooked into the sample's build) and read back;
//! the same through a live cook service; and an edit hot-reloading into the picture.

use std::path::Path;
use std::time::Duration;

use marlin_asset::Uuid;
use marlin_asset_db::{Options, Project};
use marlin_cook_core::{CookOptions, PackageOptions, cook_all, publish_manifest, write_package};
use marlin_cookd::{Cookd, CookdOptions, bind};
use marlin_cooker::{engine_cookers, open_ddc};
use marlin_framework::{
    Boot, ContentSettings, Engine, OFFSCREEN_FORMAT, Offscreen, RenderSide, load_default_bake,
    load_default_resolve, load_engine_shaders, load_pairing_pattern, mount,
};
use marlin_gpu_test_support::{Image, assert_image, hardware_device};
use marlin_render::asset::PairingPatternView;
use marlin_render_graph::{FrameContext, FrameContextDesc, TextureDesc};
use marlin_rhi::Device;
use marlin_shader::asset::ShaderLibrary;
use marlin_test_support::{BackgroundService, TempDir, wait_until};

pub(crate) const LEVEL: &str = "01992d00-0000-7000-8000-000000000001";
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
pub(crate) fn sample(name: &str) -> TempDir {
    let root = TempDir::new(&format!("e2e-{name}"));
    let sample = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../samples/basic");
    copy_dir(&sample, &root);
    root
}

/// The project at `root`, with this repository's engine (the copy is outside it).
fn project(root: &Path) -> Project {
    let engine = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    Project::open_with(root, Some(&engine)).unwrap()
}

/// Cooks and packages the project at `root`, the engine's content with it; returns the package
/// path.
pub(crate) fn package(root: &Path) -> String {
    let project = project(root);
    let content = project
        .build_content(&Options {
            journal: false,
            ..Options::default()
        })
        .unwrap();
    let ddc = open_ddc(&project.ddc_dir(), None).unwrap();
    let (report, manifest) = cook_all(
        &content,
        &engine_cookers(),
        ddc.as_ref(),
        &CookOptions::default(),
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    publish_manifest(&project.build_dir(), ddc.as_ref(), &manifest).unwrap();
    let out = root.join("content.mpak");
    write_package(&manifest, ddc.as_ref(), &PackageOptions::default(), &out).unwrap();
    out.to_string_lossy().into_owned()
}

struct Viewer {
    device: &'static Device,
    render: RenderSide,
    offscreen: Offscreen,
}

impl Viewer {
    /// The render side made with the engine's cooked shaders: its library and the default
    /// material's resolve permutation and bake, and the pairing patterns.
    fn new(
        library: &ShaderLibrary,
        resolve: &ShaderLibrary,
        bake: &ShaderLibrary,
        pairing: PairingPatternView<'_>,
        ids: marlin_render::SceneIds,
    ) -> Self {
        let device = hardware_device();
        let mut render = RenderSide::new(
            device,
            library,
            resolve,
            bake,
            pairing,
            OFFSCREEN_FORMAT,
            ids,
        )
        .unwrap();
        // These tests compare pictures across runs whose loading takes different numbers of
        // frames; TSR's history and jitter phase depend on them, so it is off here (it has tests
        // of its own, marlin-material's tsr.rs). The resize test keeps it.
        render.upscale = None;
        let offscreen = Offscreen::new(device, WIDTH, HEIGHT).unwrap();
        Self {
            render,
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

/// Loads the engine's shaders from `boot`'s content as `run` does, then its level, and runs
/// frames until the level is loaded; returns the engine, a viewer drawing with those shaders, and
/// the first complete picture.
fn start<'a>(boot: &'a Boot) -> (Engine<'a>, Viewer, Vec<u32>) {
    let mut engine = Engine::new(&boot.vfs).unwrap();
    let library = load_engine_shaders(&mut engine.assets, &boot.vfs, None).unwrap();
    let resolve = load_default_resolve(&mut engine.assets, &boot.vfs, None).unwrap();
    let bake = load_default_bake(&mut engine.assets, &boot.vfs, None).unwrap();
    let pairing = load_pairing_pattern(&mut engine.assets, &boot.vfs, None).unwrap();
    let mut viewer = Viewer::new(
        engine.assets.get(&library).unwrap(),
        engine.assets.get(&resolve).unwrap(),
        engine.assets.get(&bake).unwrap(),
        engine.assets.get(&pairing).unwrap().view(),
        engine.scene_ids(),
    );
    engine.load_level(boot.level);
    wait_until(Duration::from_secs(60), "the level to load", || {
        engine.is_ready() || {
            viewer.frame(&mut engine);
            false
        }
    });
    // Then frames until the meshes' pages have streamed in: the frame after streaming catches
    // up draws every page it wants.
    let mut frames = 0;
    loop {
        viewer.frame(&mut engine);
        frames += 1;
        if viewer.render.streaming_idle() {
            break;
        }
        assert!(frames < 64, "streaming did not catch up");
    }
    let pixels = viewer.frame(&mut engine);
    (engine, viewer, pixels)
}

fn rgb(p: u32) -> [u8; 3] {
    [p as u8, (p >> 8) as u8, (p >> 16) as u8]
}

fn at(pixels: &[u32], x: u32, y: u32) -> [u8; 3] {
    rgb(pixels[(y * WIDTH + x) as usize])
}

/// Pixels around the middle and above it (where the sphere is, parts of it behind the spinning
/// torus) that `is` holds for.
fn around_center(pixels: &[u32], is: impl Fn([u8; 3]) -> bool) -> usize {
    (HEIGHT / 2 - 16..HEIGHT / 2 + 8)
        .flat_map(|y| (WIDTH / 2 - 8..WIDTH / 2 + 8).map(move |x| at(pixels, x, y)))
        .filter(|&c| is(c))
        .count()
}

/// What the sample must look like: the same blue-black background at both top corners (the
/// clear color, exposed and tonemapped), the orange sphere in the middle, the grey ground at the
/// bottom.
fn check_sample(pixels: &[u32]) {
    let clear = at(pixels, 1, 1);
    assert!(
        clear[2] > clear[1] && clear[1] > clear[0],
        "a blue background: {clear:?}"
    );
    let other = at(pixels, WIDTH - 2, 1);
    assert!(
        other.iter().zip(clear).all(|(a, b)| a.abs_diff(b) <= 1),
        "background at both corners: {other:?} and {clear:?}"
    );
    let orange = around_center(pixels, |c| {
        c[0] > c[1] && c[1] > c[2] && c[0] > 60 && c[0] > c[2] + 40
    });
    assert!(orange > 10, "the orange sphere: {orange} pixels");
    let ground = at(pixels, WIDTH / 2, HEIGHT - 3);
    let spread = ground.iter().max().unwrap() - ground.iter().min().unwrap();
    assert!(ground[0] > 20 && spread < 12, "the grey ground: {ground:?}");
    let lit = pixels.iter().filter(|&&p| rgb(p) != clear).count();
    assert!(
        lit > pixels.len() / 3,
        "the scene covers the view: {lit} pixels"
    );
}

/// A cook service for the project at `root` on a background thread, ready for clients: cookd
/// binds its own network queue, so it reports its address once it listens.
fn start_cookd(root: &Path) -> BackgroundService {
    let root = root.to_owned();
    BackgroundService::spawn(move |ready, stop| {
        let project = project(&root);
        let (content, _) = project.open_content(&Options::default()).unwrap();
        let ddc = open_ddc(&project.ddc_dir(), None).unwrap();
        let options = CookdOptions {
            workers: 2,
            watch_interval: Duration::from_millis(30),
        };
        let mut cookd = Cookd::new(content, engine_cookers(), ddc, &options);
        let mut server = bind("127.0.0.1:0".parse().unwrap()).unwrap();
        ready.listening(server.local_addr());
        cookd.serve(&mut server, stop).unwrap();
    })
}

#[test]
fn the_sample_cooks_packages_and_renders_the_same_from_cookd() {
    let root = sample("package");
    let package = package(&root);
    // The meshes' finer pages are streamed blobs: read from the package when the renderer asks.
    let streamed = marlin_asset::Package::open(&package)
        .unwrap()
        .index()
        .blobs
        .iter()
        .filter(|b| b.flags & marlin_asset::cooked::BULK_STREAMED != 0)
        .count();
    assert!(streamed > 0, "no streamed blobs in the package");
    let level: Uuid = LEVEL.parse().unwrap();

    let boot = mount(&ContentSettings {
        packages: vec![package],
        cookd: None,
        platform: marlin_rt::PLATFORM.into(),
        level,
    })
    .unwrap();
    let (_, viewer, packaged) = start(&boot);
    check_sample(&packaged);
    assert_eq!(
        viewer.render.mesh_count(),
        3,
        "cube, sphere and torus on the GPU"
    );
    // Time stands still in these frames, so the first complete one is always the same picture.
    assert_image!(
        "sample_first_frame",
        &Image::from_words(WIDTH, HEIGHT, &packaged)
    );

    // The same content streamed from a cook service (the engine's with it): the
    // same picture, pixel for pixel.
    let cookd = start_cookd(&root);
    let boot = mount(&ContentSettings {
        packages: Vec::new(),
        cookd: Some(cookd.addr()),
        platform: marlin_rt::PLATFORM.into(),
        level,
    })
    .unwrap();
    let (_, _, streamed) = start(&boot);
    assert!(
        streamed == packaged,
        "cookd and the package render the same"
    );
    drop(cookd);
}

#[test]
fn editing_the_level_changes_the_picture_live() {
    let root = sample("reload");
    let cookd = start_cookd(&root);
    let level: Uuid = LEVEL.parse().unwrap();
    let boot = mount(&ContentSettings {
        packages: Vec::new(),
        cookd: Some(cookd.addr()),
        platform: marlin_rt::PLATFORM.into(),
        level,
    })
    .unwrap();
    let (mut engine, mut viewer, before) = start(&boot);
    check_sample(&before);

    // The sphere turns green in the text; the running game follows.
    let path = root.join("content/levels/main.masset");
    let text = std::fs::read_to_string(&path).unwrap();
    let edited = text.replace(
        "Color = (0.9, 0.55, 0.2, 1.0)",
        "Color = (0.1, 0.9, 0.2, 1.0)",
    );
    assert_ne!(edited, text);
    std::fs::write(&path, edited).unwrap();
    wait_until(Duration::from_secs(30), "the sphere to turn green", || {
        let pixels = viewer.frame(&mut engine);
        // Exposed and tonemapped, so all channels rise: green clearly ahead of the others.
        around_center(&pixels, |c| {
            let [r, g, b] = c.map(u32::from);
            g > r.max(b) + 40
        }) > 10
    });
    // Only the level reloaded; its meshes stayed on the GPU.
    assert_eq!(viewer.render.mesh_count(), 3);
    drop(cookd);
}

/// The sample's brightness over four seconds from its first frame: the exposure takes the loaded
/// scene's at once and holds it, neither fading down from a half-loaded frame nor drifting.
#[test]
fn the_exposure_holds_steady_from_the_first_frame() {
    let root = sample("exposure");
    let package = package(&root);
    let level: Uuid = LEVEL.parse().unwrap();
    let boot = mount(&ContentSettings {
        packages: vec![package],
        cookd: None,
        platform: marlin_rt::PLATFORM.into(),
        level,
    })
    .unwrap();
    let (mut engine, mut viewer, _) = start(&boot);
    let mean = |px: &[u32]| {
        px.iter()
            .map(|p| ((p & 255) + ((p >> 8) & 255) + ((p >> 16) & 255)) as f32 / 3.0)
            .sum::<f32>()
            / px.len() as f32
    };
    let mut means = Vec::new();
    for _ in 0..240 {
        let delta = engine.frame(1.0 / 60.0, None, None);
        let px = viewer
            .offscreen
            .render(viewer.device, &mut viewer.render, &delta)
            .unwrap();
        means.push(mean(&px));
    }
    let (first, last) = (means[0], means[239]);
    // Middle grey through the filmic curve, written linear: about 0.4 of full scale.
    assert!((70.0..130.0).contains(&last), "settled mean {last}");
    assert!(
        (first - last).abs() < 6.0,
        "first frame {first}, settled {last}"
    );
    let (lo, hi) = means
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), &m| (lo.min(m), hi.max(m)));
    assert!(hi - lo < 8.0, "means from {lo} to {hi}");
    // The RT LODs streamed in from the package and every mesh has its BLAS.
    assert_eq!(
        viewer.render.meshes.blas_count(),
        viewer.render.mesh_count()
    );
    drop(engine);
}

/// A window dragged from landscape to portrait: a new size every frame (each a new graph plan,
/// the cache evicting old ones), the frames in flight in between as in the game loop. Nothing
/// is destroyed while a frame using its memory runs; the validation layer stays quiet.
#[test]
fn resizing_with_frames_in_flight_keeps_validation_quiet() {
    let root = sample("resize");
    let package = package(&root);
    let level: Uuid = LEVEL.parse().unwrap();
    let boot = mount(&ContentSettings {
        packages: vec![package],
        cookd: None,
        platform: marlin_rt::PLATFORM.into(),
        level,
    })
    .unwrap();
    let device = hardware_device();
    let mut engine = Engine::new(&boot.vfs).unwrap();
    let library = load_engine_shaders(&mut engine.assets, &boot.vfs, None).unwrap();
    let resolve = load_default_resolve(&mut engine.assets, &boot.vfs, None).unwrap();
    let bake = load_default_bake(&mut engine.assets, &boot.vfs, None).unwrap();
    let pairing = load_pairing_pattern(&mut engine.assets, &boot.vfs, None).unwrap();
    let mut render = RenderSide::new(
        device,
        engine.assets.get(&library).unwrap(),
        engine.assets.get(&resolve).unwrap(),
        engine.assets.get(&bake).unwrap(),
        engine.assets.get(&pairing).unwrap().view(),
        OFFSCREEN_FORMAT,
        engine.scene_ids(),
    )
    .unwrap();
    engine.load_level(boot.level);
    let mut frames = FrameContext::new(device, &FrameContextDesc::default()).unwrap();
    let before = device.validation_errors();
    let mut frame = |size: (u32, u32), engine: &mut Engine<'_>, frames: &mut FrameContext| {
        let delta = engine.frame(1.0 / 60.0, None, None);
        render.collect_feedback(frames);
        render.apply(device, &delta).unwrap();
        frames
            .execute(device, |g, _| {
                let target = g.create_texture(
                    "Target",
                    TextureDesc::new_2d(size.0, size.1, OFFSCREEN_FORMAT),
                );
                render.add_passes(g, target);
            })
            .unwrap();
    };
    wait_until(Duration::from_secs(60), "the level to load", || {
        frame((320, 180), &mut engine, &mut frames);
        engine.is_ready()
    });
    for _ in 0..8 {
        frame((320, 180), &mut engine, &mut frames);
    }
    // As the game loop's resize: wait for the GPU, then the new size.
    for i in 0..24u32 {
        frames.wait_idle().unwrap();
        frame((320 - 6 * i, 180 + 10 * i), &mut engine, &mut frames);
    }
    for _ in 0..6 {
        frame((176, 420), &mut engine, &mut frames);
    }
    frames.wait_idle().unwrap();
    assert_eq!(device.validation_errors(), before);
}
