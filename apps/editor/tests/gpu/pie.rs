//! The editor on the sample, driven through its application interface with the engine's cooked
//! bindings: Alt+P plays the level in editor (a simulated world of its own, while the editor's
//! stands still) and stops it again.

use std::path::Path;

use marlin_app::{Size, WindowId};
use marlin_asset_db::{Options, Project};
use marlin_cook_core::{CookOptions, PackageOptions, cook_all, publish_manifest, write_package};
use marlin_cooker::{engine_cookers, open_ddc};
use marlin_editor::app::{EditorApp, EditorFrame, EditorSim, EditorStart, VIEWPORT_FORMAT};
use marlin_editor::shell;
use marlin_editor_core::SourceContent;
use marlin_framework::{
    ContentSettings, Engine, HostApp, HostSetup, SimContext, WindowState, mount,
};
use marlin_gpu_test_support::hardware_device;
use marlin_input::{InputEvent, InputState, Key, MouseButton};
use marlin_input_action::{CursorMode, InputFrame};
use marlin_jobs::{Config, JobSystem};
use marlin_math::{Quat, Vec2};
use marlin_test_support::TempDir;
use marlin_world::{Modules, Transform, World};

const LEVEL: &str = "01992d00-0000-7000-8000-000000000001";

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

/// A copy of `samples/basic` with this repository's engine, cooked and packaged: the project
/// and its package's path.
fn sample() -> (TempDir, Project, String) {
    let root = TempDir::new("editor-pie");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    copy_dir(&repo.join("samples/basic"), &root);
    let project = Project::open_with(&root, Some(&repo)).unwrap();
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
    (root, project, out.to_string_lossy().into_owned())
}

const WINDOW: WindowState = WindowState {
    id: WindowId::from_raw(1),
    size: Size {
        width: 1600,
        height: 900,
    },
    scale: 1.0,
    main: true,
    focused: true,
    minimized: false,
    cursor: CursorMode::Visible,
};

/// One editor frame, 0.1 s after the last, on `input` (what the host's commands left) with the
/// pointer as `state` has it.
fn step_with(
    sim: &mut EditorSim,
    engine: &mut Engine<'_>,
    jobs: &JobSystem,
    state: InputState,
    input: InputFrame,
) -> EditorFrame {
    EditorApp::simulate(
        sim,
        &mut SimContext {
            engine,
            jobs,
            dt: 0.1,
            interval: 0.1,
            state,
            input,
            windows: vec![WINDOW],
            closed: Vec::new(),
            requests: Vec::new(),
        },
    )
}

fn step(
    sim: &mut EditorSim,
    engine: &mut Engine<'_>,
    jobs: &JobSystem,
    input: InputFrame,
) -> EditorFrame {
    step_with(sim, engine, jobs, InputState::default(), input)
}

/// The pointer over `at` (logical pixels; the window's scale is 1), the left button just pressed.
fn click(at: Vec2) -> InputState {
    let mut state = InputState::default();
    state.begin_frame();
    state.apply(InputEvent::MouseMoved {
        window: WINDOW.id,
        x: at.x,
        y: at.y,
    });
    state.apply(InputEvent::MouseButton {
        button: MouseButton::Left,
        down: true,
    });
    state
}

/// The rotations of the world's spinning objects.
fn spins(world: &World) -> Vec<Quat> {
    world
        .ecs()
        .query_ref::<(&Transform, &marlin::gameplay::RotatingMovement)>()
        .map(|(t, _)| t.rotation)
        .collect()
}

#[test]
fn alt_p_plays_the_level_in_editor_and_stops_it() {
    let (_root, project, package) = sample();
    let boot = mount(&ContentSettings {
        packages: vec![package],
        cookd: None,
        platform: marlin_rt::PLATFORM.into(),
        level: LEVEL.parse().unwrap(),
    })
    .unwrap();
    let jobs = JobSystem::new(Config::default()).unwrap();
    let mut engine = Engine::new(
        &boot.vfs,
        Modules::new(marlin::game_modules()).unwrap(),
        Some(&jobs),
    )
    .unwrap();
    let (mut sim, _render) = EditorApp::setup(
        &mut HostSetup {
            engine: &mut engine,
            boot: &boot,
            device: hardware_device(),
            jobs: &jobs,
            output: VIEWPORT_FORMAT,
            window: WINDOW,
        },
        EditorStart {
            sources: SourceContent::open(&project, engine_cookers()).unwrap(),
            project: project.name.clone(),
            level: Some(LEVEL.parse().unwrap()),
        },
    )
    .unwrap();

    // Editing: nothing plays.
    step(&mut sim, &mut engine, &jobs, InputFrame::empty());
    assert!(sim.playing().is_none());

    // Alt+P plays: a world of its own whose spinning objects spin.
    let alt_p = || InputFrame::empty().with_key(Key::LEFT_ALT).with_key(Key::P);
    step(&mut sim, &mut engine, &jobs, alt_p());
    assert!(sim.playing().is_some(), "Alt+P starts play in editor");
    let start = spins(sim.playing().unwrap());
    assert!(!start.is_empty(), "the sample has spinning objects");
    for _ in 0..5 {
        step(&mut sim, &mut engine, &jobs, InputFrame::empty());
    }
    let now = spins(sim.playing().unwrap());
    assert!(
        start
            .iter()
            .zip(&now)
            .all(|(a, b)| !a.rotation_eq(*b, 1e-4)),
        "playing simulates: {start:?} -> {now:?}"
    );

    // Alt+P again stops.
    step(&mut sim, &mut engine, &jobs, alt_p());
    assert!(sim.playing().is_none());

    // The toolbar's play button plays, its stop button stops.
    let layout = shell::layout(Vec2::new(1600.0, 900.0));
    let center = |r: marlin_ui_render::Rect| (r.min + r.max) * 0.5;
    step_with(
        &mut sim,
        &mut engine,
        &jobs,
        click(center(layout.stop)),
        InputFrame::empty(),
    );
    assert!(sim.playing().is_none(), "stop does nothing while editing");
    step_with(
        &mut sim,
        &mut engine,
        &jobs,
        click(center(layout.play)),
        InputFrame::empty(),
    );
    assert!(sim.playing().is_some(), "the play button plays");
    step_with(
        &mut sim,
        &mut engine,
        &jobs,
        click(center(layout.play)),
        InputFrame::empty(),
    );
    assert!(sim.playing().is_some(), "play again keeps playing");
    step_with(
        &mut sim,
        &mut engine,
        &jobs,
        click(center(layout.stop)),
        InputFrame::empty(),
    );
    assert!(sim.playing().is_none(), "the stop button stops");
}
