//! The game ([`GameApp`]): one game world with the entry level, drawn into the whole main window by
//! one world renderer, the window being one game viewport (0019 §9.3).

use marlin_diag::info;
use marlin_render_graph::RenderGraph;
use marlin_renderer::{RenderDelta, WorldRenderer};
use marlin_world::{Modules, Transform, World, WorldKind};

use crate::boot::{Boot, WINDOW_HEIGHT, WINDOW_WIDTH};
use crate::host::{
    HostApp, HostOptions, HostSetup, RenderContext, RunError, SimContext, WindowTarget, run_app,
};
use crate::input::{ViewportEnv, ViewportInput, give_player_input, input_settings};

/// The game's simulation side.
pub struct GameSim {
    pub world: World,
    viewport: ViewportInput,
    /// The level's load has been reported.
    reported: bool,
}

/// The game as a host application.
pub struct GameApp;

impl HostApp for GameApp {
    type Sim = GameSim;
    type Frame = Option<RenderDelta>;
    type Render = WorldRenderer;

    fn simulate(sim: &mut GameSim, cx: &mut SimContext<'_, '_>) -> Option<RenderDelta> {
        let settings = input_settings(&sim.world);
        let main = *cx.main_window();
        let env = ViewportEnv {
            hovered: true,
            focused: main.focused,
            text_focus: false,
            release: false,
        };
        let route = sim
            .viewport
            .route(&cx.state, core::mem::take(&mut cx.input), env, settings);
        cx.windows[0].cursor = route.cursor;
        give_player_input(&mut sim.world, 0, route.world);
        cx.engine.frame(&mut sim.world, cx.dt, Some(cx.jobs));
        if !sim.reported && cx.engine.is_ready(&sim.world) {
            sim.reported = true;
            info!(
                "level loaded: {} entities, {} drawn",
                sim.world.ecs().entity_count(),
                marlin_renderer::instance_count(&sim.world)
            );
        }
        marlin_renderer::take_delta(&mut sim.world)
    }

    fn prepare(
        render: &mut WorldRenderer,
        cx: &mut RenderContext<'_>,
        frame: &Option<RenderDelta>,
    ) -> Result<(), RunError> {
        render.collect_feedback(cx.frames);
        if let Some(delta) = frame {
            render.apply(cx.device, delta)?;
        }
        // The quality cvars (0008 §2), which the console may have changed.
        marlin_renderer::settings::RenderSettings::from_cvars().apply(render);
        Ok(())
    }

    fn render<'g>(
        render: &'g mut WorldRenderer,
        _: &'g Option<RenderDelta>,
        g: &mut RenderGraph<'g>,
        windows: &mut [WindowTarget<'g>],
    ) {
        // The game draws into its main window only.
        if let Some(main) = windows.iter_mut().find(|w| w.main) {
            main.image = render.add_passes(g, main.image);
        }
    }
}

/// Runs the game of `modules` (registered before `boot` read its configuration) in a window until
/// it is closed (or the host's quit command): a game world with the entry level, drawn by a
/// [`WorldRenderer`].
///
/// The host's commands are input actions (`content/engine/input/Host.masset`): F11 or Alt+Enter,
/// borderless fullscreen; and in development builds Esc or gamepad Start, quit; V, vsync; F3, the
/// stats overlay. The rest reaches player 0 as the world's input settings say
/// ([`crate::input`]).
pub fn run(boot: &Boot, title: &str, modules: Modules) -> Result<(), RunError> {
    let options = HostOptions {
        title,
        width: WINDOW_WIDTH.get() as u32,
        height: WINDOW_HEIGHT.get() as u32,
        max_frames: None,
        capture_last_frame: false,
    };
    run_app::<GameApp>(boot, modules, &options, |setup: &mut HostSetup<'_, '_>| {
        let mut world = setup.engine.create_world(WorldKind::Game, Some(setup.jobs));
        world.load_sublevel(setup.boot.level, Transform::IDENTITY);
        let render = WorldRenderer::new(
            setup.device,
            &setup.render_shaders()?,
            setup.output,
            marlin_renderer::scene_ids(&world).ok_or(RunError::NoRenderer)?,
        )?;
        Ok((
            GameSim {
                world,
                viewport: ViewportInput::game(),
                reported: false,
            },
            render,
        ))
    })?;
    Ok(())
}
