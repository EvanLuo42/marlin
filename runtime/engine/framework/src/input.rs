//! Input routing (0019 §9.2, §9.3): raw input passes the host's own commands, then the UI, then
//! a viewport, and what is left reaches the world's players.
//!
//! - [`HostInput`]: the host's command contexts (`Marlin.Engine.Input.Host` in every build,
//!   `Marlin.Engine.Input.Dev` in development builds), evaluated first; what they fire with,
//!   nobody else sees.
//! - [`ViewportInput`]: one viewport's state machine, deciding whether its world gets the
//!   input and what the cursor does. A game's window is one viewport that always has the input
//!   ([`ViewportInput::game`]); an editor's scene view takes it while the right button flies the
//!   camera ([`ViewportInput::editor`]).

use alloc::sync::Arc;

use marlin_app::{CursorGrab, Window};
use marlin_asset::{AssetServer, Uuid, Vfs};
use marlin_input::{InputState, MouseButton};
use marlin_input_action::{
    CompiledContext, CursorMode, Devices, InputFrame, InputMode, InputSettings, InputUser,
    load_context,
};
use marlin_jobs::JobSystem;

/// The host's commands this frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct HostCommands {
    pub quit: bool,
    pub fullscreen: bool,
    pub vsync: bool,
    pub stats: bool,
}

/// The host's input user: its command contexts and their actions.
pub struct HostInput {
    user: InputUser,
    quit: Option<Uuid>,
    fullscreen: Option<Uuid>,
    vsync: Option<Uuid>,
    stats: Option<Uuid>,
}

impl HostInput {
    /// The host contexts from `vfs`, loaded through `assets` (with the input module's loaders),
    /// blocking. Content without them gives a host without commands, which says so.
    pub fn load(assets: &mut AssetServer, vfs: &Vfs, jobs: Option<&JobSystem>) -> Self {
        let mut user = InputUser::new();
        let mut add = |name: &str, priority: i32| match load_context(assets, vfs, jobs, name) {
            Ok(context) => user.add_context(context, priority),
            Err(e) => marlin_diag::warn!("input context `{name}`: {e:?}; its commands are off"),
        };
        add("Marlin.Engine.Input.Host", 100);
        #[cfg(not(marlin_build = "shipping"))]
        add("Marlin.Engine.Input.Dev", 90);
        Self {
            user,
            quit: vfs.named("Marlin.Engine.Input.Quit"),
            fullscreen: vfs.named("Marlin.Engine.Input.Fullscreen"),
            vsync: vfs.named("Marlin.Engine.Input.Vsync"),
            stats: vfs.named("Marlin.Engine.Input.Stats"),
        }
    }

    /// A host with the given contexts (tests, tools).
    pub fn with_contexts(contexts: &[Arc<CompiledContext>], vfs: &Vfs) -> Self {
        let mut user = InputUser::new();
        for (i, c) in contexts.iter().enumerate() {
            user.add_context(c.clone(), 100 - i as i32);
        }
        Self {
            user,
            quit: vfs.named("Marlin.Engine.Input.Quit"),
            fullscreen: vfs.named("Marlin.Engine.Input.Fullscreen"),
            vsync: vfs.named("Marlin.Engine.Input.Vsync"),
            stats: vfs.named("Marlin.Engine.Input.Stats"),
        }
    }

    /// Evaluates the host's contexts on `frame` (`dt` seconds after the last); returns the
    /// commands and the frame without what they used.
    pub fn route(&mut self, frame: InputFrame, dt: f32) -> (HostCommands, InputFrame) {
        let consumed = self.user.evaluate(&frame, dt);
        let fired = |id: Option<Uuid>| id.is_some_and(|id| self.user.action(id).triggered);
        let commands = HostCommands {
            quit: fired(self.quit),
            fullscreen: fired(self.fullscreen),
            vsync: fired(self.vsync),
            stats: fired(self.stats),
        };
        let mut rest = frame;
        for s in &consumed {
            rest.consume(s);
        }
        (commands, rest)
    }
}

/// Where a viewport is in taking the input (0019 §9.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewportState {
    /// The pointer may hover; nothing goes to the world.
    Unfocused,
    /// Clicked: the editor's viewport tools have it (selection, gizmos); not the world.
    Focused,
    /// The right button is held over an editor viewport: the world's camera flies, the cursor
    /// is locked; released, back to focused.
    Captured,
    /// A game's viewport (or play in editor): the world gets everything the layers above left.
    GameCapture,
}

/// What a viewport does with this frame's input.
#[derive(Clone, PartialEq, Debug)]
pub struct ViewportRoute {
    /// What the viewport's world gets (player 0).
    pub world: InputFrame,
    pub cursor: CursorMode,
}

/// One viewport's input state machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ViewportInput {
    state: ViewportState,
    game: bool,
}

impl ViewportInput {
    /// A game's viewport: it has the input from the start.
    pub const fn game() -> Self {
        Self {
            state: ViewportState::GameCapture,
            game: true,
        }
    }

    /// An editor's scene viewport: focused by a click, captured while the right button is held.
    pub const fn editor() -> Self {
        Self {
            state: ViewportState::Unfocused,
            game: false,
        }
    }

    pub fn state(&self) -> ViewportState {
        self.state
    }

    /// Advances the state on `state`'s buttons and routes `frame` (what the layers above left):
    /// `hovered`, the pointer is over the viewport; `text_focus`, a text field has the keyboard;
    /// `release`, the host's release command (play in editor: give the mouse back); `settings`,
    /// the world's input mode and cursor.
    pub fn route(
        &mut self,
        state: &InputState,
        frame: InputFrame,
        hovered: bool,
        text_focus: bool,
        release: bool,
        settings: InputSettings,
    ) -> ViewportRoute {
        let mouse = state.mouse();
        let any_press = [MouseButton::Left, MouseButton::Right, MouseButton::Middle]
            .iter()
            .any(|&b| mouse.pressed(b));
        let right_down = mouse.down(MouseButton::Right) || mouse.pressed(MouseButton::Right);
        self.state = match self.state {
            _ if release && self.state == ViewportState::GameCapture => ViewportState::Focused,
            ViewportState::Unfocused | ViewportState::Focused if any_press && hovered => {
                if self.game {
                    ViewportState::GameCapture
                } else if mouse.pressed(MouseButton::Right) {
                    ViewportState::Captured
                } else {
                    ViewportState::Focused
                }
            }
            ViewportState::Focused if any_press => ViewportState::Unfocused,
            ViewportState::Captured if !right_down => ViewportState::Focused,
            s => s,
        };
        let world = match self.state {
            ViewportState::Unfocused | ViewportState::Focused => InputFrame::empty(),
            ViewportState::Captured => frame,
            ViewportState::GameCapture => match settings.mode {
                InputMode::UiOnly => InputFrame::empty(),
                InputMode::GameOnly | InputMode::GameAndUi => frame,
            },
        };
        let world = if text_focus {
            world.without_keyboard()
        } else {
            world
        };
        let cursor = match self.state {
            ViewportState::Captured => CursorMode::Locked,
            ViewportState::GameCapture if settings.mode != InputMode::UiOnly => settings.cursor,
            _ => CursorMode::Visible,
        };
        ViewportRoute { world, cursor }
    }
}

/// The raw input of the keyboard, the mouse and the first gamepad: what player 0 reads.
pub fn player_zero(state: &InputState) -> InputFrame {
    InputFrame::from_state(
        state,
        Devices {
            keyboard_mouse: true,
            gamepad: Some(0),
        },
    )
}

/// Makes `window`'s cursor do what `mode` says, when it changed since `last`.
pub fn apply_cursor(window: &Window, mode: CursorMode, last: &mut Option<CursorMode>) {
    if *last == Some(mode) {
        return;
    }
    *last = Some(mode);
    match mode {
        CursorMode::Visible => {
            window.set_cursor_grab(CursorGrab::None);
            window.set_cursor_visible(true);
        }
        CursorMode::Hidden => {
            window.set_cursor_grab(CursorGrab::None);
            window.set_cursor_visible(false);
        }
        CursorMode::Locked => {
            window.set_cursor_grab(CursorGrab::Lock);
            window.set_cursor_visible(false);
        }
    }
}

/// Gives `frame` to player `index` of `world` for its next frame.
pub fn give_player_input(world: &mut marlin_world::World, index: usize, frame: InputFrame) {
    if let Some(players) = world
        .ecs_mut()
        .resource_mut::<marlin_input_action::Players>()
    {
        players.set_input(index, frame);
    }
}

/// The world's input settings (defaults if it has none).
pub fn input_settings(world: &marlin_world::World) -> InputSettings {
    world
        .ecs()
        .resource::<InputSettings>()
        .copied()
        .unwrap_or_default()
}
