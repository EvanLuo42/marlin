//! Input routing: the host's commands consume their input, and viewports take the input by
//! their state machine.

use std::sync::Arc;

use marlin_asset::{Link, Uuid, Vfs};
use marlin_framework::input::{HostInput, ViewportEnv, ViewportInput, ViewportState, player_zero};
use marlin_input::{InputEvent, InputState, Key, MouseButton};
use marlin_input_action::{
    CompiledContext, CursorMode, InputFrame, InputMode, InputSettings, InputSource, KeyCode,
    Mapping, MappingContext, Trigger,
};

/// The input after `events`, as a frame begun on the last frame's state.
fn after(state: &mut InputState, events: &[InputEvent]) {
    state.begin_frame();
    for &e in events {
        state.apply(e);
    }
}

fn button(button: MouseButton, down: bool) -> InputEvent {
    InputEvent::MouseButton { button, down }
}

/// A focused window's viewport.
fn env(hovered: bool, text_focus: bool, release: bool) -> ViewportEnv {
    ViewportEnv {
        hovered,
        focused: true,
        text_focus,
        release,
    }
}

#[test]
fn editor_viewports_capture_while_the_right_button_flies() {
    let mut state = InputState::default();
    let mut v = ViewportInput::editor();
    let settings = InputSettings::default();
    let w = InputFrame::empty().with_key(Key::W);

    // Hovering gives the world nothing.
    after(&mut state, &[]);
    let r = v.route(&state, w.clone(), env(true, false, false), settings);
    assert_eq!(v.state(), ViewportState::Unfocused);
    assert_eq!(r.world, InputFrame::empty());

    // A left click focuses; still nothing for the world.
    after(&mut state, &[button(MouseButton::Left, true)]);
    v.route(&state, w.clone(), env(true, false, false), settings);
    assert_eq!(v.state(), ViewportState::Focused);
    after(&mut state, &[button(MouseButton::Left, false)]);

    // The right button captures: locked cursor, the world flies.
    after(&mut state, &[button(MouseButton::Right, true)]);
    let r = v.route(&state, w.clone(), env(true, false, false), settings);
    assert_eq!(v.state(), ViewportState::Captured);
    assert_eq!(r.cursor, CursorMode::Locked);
    assert!(r.world.key(Key::W));

    // Released: back to focused, cursor back.
    after(&mut state, &[button(MouseButton::Right, false)]);
    let r = v.route(&state, w.clone(), env(true, false, false), settings);
    assert_eq!(v.state(), ViewportState::Focused);
    assert_eq!(r.cursor, CursorMode::Visible);

    // A click elsewhere unfocuses.
    after(&mut state, &[button(MouseButton::Left, true)]);
    v.route(&state, w, env(false, false, false), settings);
    assert_eq!(v.state(), ViewportState::Unfocused);
}

#[test]
fn game_viewports_follow_the_input_mode_and_give_the_mouse_back() {
    let state = InputState::default();
    let mut v = ViewportInput::game();
    let w = InputFrame::empty().with_key(Key::W);
    let locked = InputSettings {
        mode: InputMode::GameOnly,
        cursor: CursorMode::Locked,
    };
    let r = v.route(&state, w.clone(), env(true, false, false), locked);
    assert_eq!(v.state(), ViewportState::GameCapture);
    assert_eq!(r.cursor, CursorMode::Locked);
    assert!(r.world.key(Key::W));

    // A menu: UI only, the cursor shown.
    let menu = InputSettings {
        mode: InputMode::UiOnly,
        cursor: CursorMode::Locked,
    };
    let r = v.route(&state, w.clone(), env(true, false, false), menu);
    assert_eq!(r.world, InputFrame::empty());
    assert_eq!(r.cursor, CursorMode::Visible);

    // A text field takes the keyboard.
    let r = v.route(&state, w.clone(), env(true, true, false), locked);
    assert!(!r.world.key(Key::W));

    // Released (play in editor): focused, the cursor back, the world gets nothing.
    let r = v.route(&state, w, env(true, false, true), locked);
    assert_eq!(v.state(), ViewportState::Focused);
    assert_eq!(r.cursor, CursorMode::Visible);
    assert_eq!(r.world, InputFrame::empty());
}

#[test]
fn host_commands_consume_their_keys() {
    let quit = Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_0a01);
    let context = CompiledContext::compile(
        Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_0a02),
        &MappingContext {
            mappings: vec![Mapping {
                action: Link::new(quit),
                source: InputSource::Key(KeyCode(Key::ESCAPE)),
                modifiers: Vec::new(),
                triggers: vec![Trigger::Pressed],
            }],
        },
        |_| None,
    );
    // Without content naming the actions, commands are off but routing still consumes.
    let mut host = HostInput::with_contexts(&[Arc::new(context)], &Vfs::new());
    let mut state = InputState::default();
    after(
        &mut state,
        &[InputEvent::Key {
            key: Key::ESCAPE,
            down: true,
            repeat: false,
        }],
    );
    let (commands, rest) = host.route(player_zero(&state), 0.016);
    assert!(!commands.quit, "the content does not name the quit action");
    assert!(
        !rest.key(Key::ESCAPE),
        "the host fired with Escape: the world does not see it"
    );
    assert!(player_zero(&state).key(Key::ESCAPE));
}

#[test]
fn losing_the_window_focus_releases_captures() {
    let mut state = InputState::default();
    let settings = InputSettings::default();
    let w = InputFrame::empty().with_key(Key::W);
    let unfocused = ViewportEnv {
        hovered: true,
        ..ViewportEnv::default()
    };

    let mut editor = ViewportInput::editor();
    after(&mut state, &[button(MouseButton::Right, true)]);
    editor.route(&state, w.clone(), env(true, false, false), settings);
    assert_eq!(editor.state(), ViewportState::Captured);
    let r = editor.route(&state, w.clone(), unfocused, settings);
    assert_eq!(editor.state(), ViewportState::Focused);
    assert_eq!(r.cursor, CursorMode::Visible);
    assert_eq!(r.world, InputFrame::empty());

    // A game keeps its viewport, without input or a locked cursor until the window is back.
    let mut game = ViewportInput::game();
    let locked = InputSettings {
        mode: InputMode::GameOnly,
        cursor: CursorMode::Locked,
    };
    let r = game.route(&state, w.clone(), unfocused, locked);
    assert_eq!(game.state(), ViewportState::GameCapture);
    assert_eq!(
        (r.cursor, r.world),
        (CursorMode::Visible, InputFrame::empty())
    );
    let r = game.route(&state, w, env(true, false, false), locked);
    assert_eq!(r.cursor, CursorMode::Locked);
}
