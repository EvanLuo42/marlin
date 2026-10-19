//! The editor as a host application (0019 §13): [`EditorApp`].
//!
//! The simulation side holds the project's sources, the open level document, the editor world
//! that shows it (not simulated), the scene viewport with its own camera, a play-in-editor
//! session when one runs, and records the window's UI. The render side holds a world renderer
//! for the editor world and one for the play session, and the window's UI renderer, which draws
//! the scene viewport's image as an external image (0008 §6.1).

use std::sync::Arc;

use marlin_asset::{InPlace, Uuid};
use marlin_editor_core::{LevelDocument, SourceContent};
use marlin_editor_viewport::{PieSession, SceneViewport, ViewportCamera, ViewportMode};
use marlin_font::{Font, GlyphIds};
use marlin_framework::input::{ViewportEnv, give_player_input, input_settings};
use marlin_framework::{HostApp, HostSetup, RenderContext, RunError, SimContext, WindowTarget};
use marlin_gameplay::SpectatorActions;
use marlin_input::MouseButton;
use marlin_input_action::{InputSettings, InputUser, load_context};
use marlin_math::{DVec3, Vec2};
use marlin_render_graph::{RenderGraph, TextureDesc};
use marlin_renderer::{RenderDelta, RenderShaders, ViewId, WorldRenderer};
use marlin_rhi::Format;
use marlin_ui_render::{
    Color, ExternalImageId, UiAssets, UiDrawList, UiPrepared, UiRenderer, UiRendererDesc,
    UiShaders, UiSurface, UiSurfaceDesc,
};
use marlin_world::{SublevelId, Transform, World, WorldKind};

use crate::shell;

/// The external image the scene viewport is drawn as.
const VIEWPORT: ExternalImageId = ExternalImageId(1);
/// The scene viewport's image: display-referred, sRGB, as the post chain writes a window.
pub const VIEWPORT_FORMAT: Format = Format::Rgba8UnormSrgb;
/// How often the sources are checked for edits made outside the editor, in seconds.
const SOURCE_CHECK: f32 = 0.5;

/// The editor's commands (`content/engine/input/Editor.masset`).
struct Commands {
    user: InputUser,
    play: Option<Uuid>,
    release: Option<Uuid>,
}

/// The editor's simulation side.
pub struct EditorSim {
    sources: SourceContent,
    project: String,
    document: Option<LevelDocument>,
    /// Where the document is spawned in the editor world.
    sublevel: Option<SublevelId>,
    world: World,
    viewport: Option<SceneViewport>,
    pie: Option<PieSession>,
    commands: Commands,
    glyph_ids: Arc<GlyphIds>,
    font: InPlace<Font>,
    status: String,
    /// Frames and seconds since the frame rate was last shown.
    fps: (u32, f32),
    /// Seconds until the next source check.
    source_check: f32,
}

/// A play session's part of a frame.
pub struct PieFrame {
    /// Its world just started: the render side makes a renderer for it.
    started: Option<marlin_renderer::SceneIds>,
    delta: Option<RenderDelta>,
}

/// One editor frame for the render side.
pub struct EditorFrame {
    ui: UiDrawList,
    /// The editor world's delta (every frame: changes must not be skipped while playing).
    editor: Option<RenderDelta>,
    pie: Option<PieFrame>,
    /// The scene viewport: its image size and what it shows (`None`: the play session's
    /// main view).
    viewport: Option<([u32; 2], Option<ViewId>)>,
}

/// The editor's render side.
pub struct EditorRender {
    shaders: RenderShaders,
    ui: UiRenderer,
    window_ui: UiSurface,
    font: InPlace<Font>,
    prepared: Option<UiPrepared>,
    editor: Option<WorldRenderer>,
    pie: Option<WorldRenderer>,
    viewport_version: u64,
}

/// The editor as a host application.
pub struct EditorApp;

/// What the editor opens: the project's sources and, if given, a level.
pub struct EditorStart {
    pub sources: SourceContent,
    pub project: String,
    pub level: Option<Uuid>,
}

impl EditorApp {
    /// Makes the editor's two sides.
    pub fn setup(
        setup: &mut HostSetup<'_, '_>,
        start: EditorStart,
    ) -> Result<(EditorSim, EditorRender), RunError> {
        let fail = |what: &str, e: String| RunError::App(format!("{what}: {e}"));
        let shaders = setup.render_shaders()?;
        let font = setup
            .engine
            .resources()
            .get::<UiAssets>()
            .ok_or_else(|| RunError::App("the engine has no UI module".into()))?
            .font
            .clone();
        let vfs = setup.engine.vfs();
        let jobs = Some(setup.jobs);

        let mut world = setup.engine.create_world(WorldKind::Editor, jobs);
        let document = start
            .level
            .map(|uuid| LevelDocument::open(&start.sources, uuid))
            .transpose()
            .map_err(|e| fail("opening the level", e))?;
        let sublevel = match &document {
            Some(d) => {
                let cooked = d.cook().map_err(|e| fail("the level", e.to_string()))?;
                Some(world.spawn_level(Arc::new(cooked), Transform::IDENTITY))
            }
            None => None,
        };

        // The viewport's camera flies by the engine's spectator bindings, starting where the
        // level's camera is.
        let viewport = match (&document, SpectatorActions::find(vfs)) {
            (Some(_), Some(actions)) => {
                let context = load_context(
                    &mut setup.engine.assets,
                    vfs,
                    jobs,
                    SpectatorActions::CONTEXT,
                )
                .map_err(|e| fail("the spectator bindings", format!("{e:?}")))?;
                let at = world
                    .ecs()
                    .query_ref::<(&Transform, &marlin_renderer::Camera)>()
                    .next()
                    .map(|(t, _)| *t)
                    .unwrap_or(Transform::from_translation(DVec3::new(0.0, 2.0, 8.0)));
                Some(SceneViewport::new(
                    0,
                    ViewportCamera::new(context, actions, at),
                ))
            }
            _ => None,
        };
        let mut commands = Commands {
            user: InputUser::new(),
            play: vfs.named("Marlin.Editor.Input.Play"),
            release: vfs.named("Marlin.Editor.Input.Release"),
        };
        match load_context(
            &mut setup.engine.assets,
            vfs,
            jobs,
            "Marlin.Editor.Input.Editor",
        ) {
            Ok(context) => commands.user.add_context(context, 0),
            Err(e) => marlin_diag::warn!("the editor's bindings: {e:?}; its commands are off"),
        }

        let ui_shaders =
            UiShaders::bind(&shaders.library).map_err(|e| fail("the UI shaders", e.to_string()))?;
        let ui = UiRenderer::new(
            setup.device,
            &ui_shaders,
            &UiRendererDesc {
                output_format: Some(setup.output),
                ..UiRendererDesc::default()
            },
        )
        .map_err(|e| fail("the UI renderer", e.to_string()))?;
        let window_ui = UiSurface::new(setup.device, &UiSurfaceDesc::new(1))
            .map_err(|e| fail("the window's UI surface", e.to_string()))?;
        let editor = match &document {
            Some(_) => Some(WorldRenderer::new(
                setup.device,
                &shaders,
                VIEWPORT_FORMAT,
                marlin_renderer::scene_ids(&world).ok_or(RunError::NoRenderer)?,
            )?),
            None => None,
        };
        let glyph_ids = ui.glyph_cache().ids.clone();
        Ok((
            EditorSim {
                sources: start.sources,
                project: start.project,
                document,
                sublevel,
                world,
                viewport,
                pie: None,
                commands,
                glyph_ids,
                font: font.clone(),
                status: String::from("Ready"),
                fps: (0, 0.0),
                source_check: SOURCE_CHECK,
            },
            EditorRender {
                shaders,
                ui,
                window_ui,
                font,
                prepared: None,
                editor,
                pie: None,
                viewport_version: 0,
            },
        ))
    }
}

impl EditorSim {
    /// The play-in-editor session's world, while one runs.
    pub fn playing(&self) -> Option<&World> {
        self.pie.as_ref().map(|p| &p.world)
    }

    /// Rereads the document if a file it came from changed, and shows it anew.
    fn reload_if_stale(&mut self, interval: f32) {
        self.source_check -= interval;
        if self.source_check > 0.0 {
            return;
        }
        self.source_check = SOURCE_CHECK;
        match self.sources.update() {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                marlin_diag::warn!("{e}");
                return;
            }
        }
        let Some(document) = &self.document else {
            return;
        };
        if !document.is_stale(&self.sources) {
            return;
        }
        let reopened = LevelDocument::open(&self.sources, document.uuid)
            .and_then(|d| Ok((d.cook().map_err(|e| e.to_string())?, d)));
        match reopened {
            Ok((cooked, document)) => {
                if let Some(old) = self.sublevel.take() {
                    self.world.unload_sublevel(old);
                }
                self.sublevel = Some(
                    self.world
                        .spawn_level(Arc::new(cooked), Transform::IDENTITY),
                );
                self.document = Some(document);
            }
            Err(e) => marlin_diag::warn!("the level changed but does not read: {e}"),
        }
    }

    /// Starts or ends play in editor.
    fn toggle_play(&mut self, cx: &mut SimContext<'_, '_>) -> Option<marlin_renderer::SceneIds> {
        let (Some(viewport), Some(document)) = (&mut self.viewport, &self.document) else {
            return None;
        };
        if self.pie.take().is_some() {
            viewport.stop();
            return None;
        }
        match PieSession::start(cx.engine, document, Some(cx.jobs)) {
            Ok(session) => {
                let ids = marlin_renderer::scene_ids(&session.world);
                self.pie = Some(session);
                viewport.play();
                ids
            }
            Err(e) => {
                marlin_diag::warn!("play in editor: {e}");
                None
            }
        }
    }
}

impl HostApp for EditorApp {
    type Sim = EditorSim;
    type Frame = EditorFrame;
    type Render = EditorRender;

    fn simulate(sim: &mut EditorSim, cx: &mut SimContext<'_, '_>) -> EditorFrame {
        sim.reload_if_stale(cx.interval);
        let main = *cx.main_window();
        let scale = main.scale;
        let logical = Vec2::new(
            main.size.width as f32 / scale,
            main.size.height as f32 / scale,
        );
        let cursor = cx
            .state
            .mouse()
            .position()
            .filter(|_| cx.state.mouse().window() == Some(main.id))
            .map(|[x, y]| Vec2::new(x / scale, y / scale));
        let layout = shell::layout(logical);
        let rect = layout.viewport;

        // The editor's commands, then the scene viewport, then the played world's player 0.
        let consumed = sim.commands.user.evaluate(&cx.input, cx.interval);
        let mut rest = core::mem::take(&mut cx.input);
        for s in &consumed {
            rest.consume(s);
        }
        let fired = |id: Option<Uuid>| id.is_some_and(|id| sim.commands.user.action(id).triggered);
        let (mut play, release) = (fired(sim.commands.play), fired(sim.commands.release));
        // The toolbar's play button plays, its stop button stops: the same command.
        if cx.state.mouse().pressed(MouseButton::Left) {
            let button = if sim.pie.is_some() {
                layout.stop
            } else {
                layout.play
            };
            play |= shell::contains(button, cursor);
        }
        let started = if play { sim.toggle_play(cx) } else { None };

        let viewport_size = [
            ((rect.width() * scale) as u32).max(1),
            ((rect.height() * scale) as u32).max(1),
        ];
        let mut shown = None;
        if let Some(viewport) = &mut sim.viewport {
            let env = ViewportEnv {
                hovered: shell::contains(rect, cursor),
                focused: main.focused,
                text_focus: false,
                release,
            };
            let settings = sim
                .pie
                .as_ref()
                .map_or(InputSettings::default(), |p| input_settings(&p.world));
            let out = viewport.route(&cx.state, rest, env, settings, cx.interval);
            cx.windows[0].cursor = out.cursor;
            if let Some(pie) = &mut sim.pie {
                give_player_input(&mut pie.world, 0, out.world);
            }
            shown = Some(match viewport.mode() {
                ViewportMode::Edit => Some(viewport.view_id()),
                ViewportMode::Play => None,
            });
        }

        // The editor world (placed and drawn, not simulated), with the viewport's camera as one
        // of its views; and the played world.
        cx.engine.frame(&mut sim.world, cx.dt, Some(cx.jobs));
        let mut editor = marlin_renderer::take_delta(&mut sim.world);
        if let (Some(delta), Some(viewport)) = (&mut editor, &sim.viewport) {
            delta.views.insert(
                0,
                viewport.camera.view(viewport.view_id(), viewport.target()),
            );
        }
        let pie = sim.pie.as_mut().map(|session| {
            cx.engine.frame(&mut session.world, cx.dt, Some(cx.jobs));
            PieFrame {
                started,
                delta: marlin_renderer::take_delta(&mut session.world),
            }
        });

        // The status line, then the window's UI.
        sim.fps.0 += 1;
        sim.fps.1 += cx.interval;
        if sim.fps.1 >= 1.0 {
            let mode = match &sim.pie {
                Some(_) => "Playing  ·  Shift+F1 frees the mouse, Stop or Alt+P stops",
                None => "Editing  ·  Play or Alt+P plays",
            };
            sim.status = format!(
                "{mode}  ·  {:.0} fps  ·  {}",
                sim.fps.0 as f32 / sim.fps.1,
                sim.project
            );
            sim.fps = (0, 0.0);
        }
        let font = sim.font.view();
        let fonts = [font];
        let mut ui = UiDrawList::new(scale);
        shell::draw(
            &mut ui.painter(&sim.glyph_ids).with_fonts(&fonts),
            font,
            &shell::ShellState {
                size: logical,
                cursor,
                status: &sim.status,
                viewport: sim.viewport.is_some().then_some(VIEWPORT),
                playing: sim.pie.is_some(),
            },
        );
        EditorFrame {
            ui,
            editor,
            pie,
            viewport: shown.map(|view| (viewport_size, view)),
        }
    }

    fn prepare(
        render: &mut EditorRender,
        cx: &mut RenderContext<'_>,
        frame: &EditorFrame,
    ) -> Result<(), RunError> {
        if let Some(r) = &mut render.editor {
            r.collect_feedback(cx.frames);
            if let Some(delta) = &frame.editor {
                r.apply(cx.device, delta)?;
            }
        }
        match &frame.pie {
            Some(pie) => {
                if let Some(ids) = &pie.started {
                    render.pie = Some(WorldRenderer::new(
                        cx.device,
                        &render.shaders,
                        VIEWPORT_FORMAT,
                        ids.clone(),
                    )?);
                }
                if let Some(r) = &mut render.pie {
                    r.collect_feedback(cx.frames);
                    if let Some(delta) = &pie.delta {
                        r.apply(cx.device, delta)?;
                    }
                }
            }
            None => {
                if render.pie.is_some() {
                    // The session ended: its frames finish before its resources go.
                    cx.frames.wait_idle()?;
                    render.pie = None;
                }
            }
        }
        let settings = marlin_renderer::settings::RenderSettings::from_cvars();
        for r in render.editor.iter_mut().chain(render.pie.iter_mut()) {
            settings.apply(r);
        }
        if frame.viewport.is_some() {
            render.viewport_version += 1;
        }
        let externals = [(VIEWPORT, render.viewport_version)];
        render.prepared = Some(render.ui.prepare(
            &[&frame.ui],
            &externals,
            &[render.font.view()],
            Some(cx.jobs),
        ));
        Ok(())
    }

    fn render<'g>(
        render: &'g mut EditorRender,
        frame: &'g EditorFrame,
        g: &mut RenderGraph<'g>,
        windows: &mut [WindowTarget<'g>],
    ) {
        let EditorRender {
            ui,
            window_ui,
            prepared,
            editor,
            pie,
            ..
        } = render;
        let Some(prepared) = prepared.as_ref() else {
            return;
        };
        let Some(main) = windows.iter_mut().find(|w| w.main) else {
            return;
        };
        // The scene viewport: the editor world through its camera, or the played world.
        let scene = frame.viewport.and_then(|([w, h], view)| {
            let renderer = match view {
                Some(_) => editor.as_mut()?,
                None => pie.as_mut()?,
            };
            let target = g.create_texture("Viewport", TextureDesc::new_2d(w, h, VIEWPORT_FORMAT));
            Some(match view {
                Some(id) => renderer.add_view_passes(g, Some(id), target),
                None => renderer.add_passes(g, target),
            })
        });
        let images: Vec<(ExternalImageId, _)> = scene.map(|t| (VIEWPORT, t)).into_iter().collect();
        let data = &prepared.surfaces[0];
        let pages = ui.begin(g, prepared);
        let out = ui.add_passes(
            g,
            pages,
            window_ui,
            data,
            main.size.width,
            main.size.height,
            Color::BLACK,
            &images,
        );
        main.image = ui.encode(g, out.target, main.image);
    }

    fn shutdown(render: &mut EditorRender, frames: &mut marlin_render_graph::FrameContext) {
        render.ui.forget(frames);
        render.window_ui.forget(frames);
    }
}
