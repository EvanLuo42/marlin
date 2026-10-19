//! A scene viewport: editing, its input flies its own camera and the world gets none; playing,
//! the world gets it until Shift+F1 releases it.

use std::sync::Arc;

use marlin_asset::{Link, Uuid};
use marlin_editor_viewport::{SceneViewport, ViewportCamera, ViewportMode};
use marlin_framework::input::{ViewportEnv, ViewportState};
use marlin_gameplay::SpectatorActions;
use marlin_input::{InputEvent, InputState, Key, MouseButton};
use marlin_input_action::{
    ActionValueType, CompiledContext, CursorMode, InputAction, InputFrame, InputSettings,
    InputSource, KeyCode, Mapping, MappingContext, Modifier, Mouse, SwizzleOrder, Trigger,
};
use marlin_math::DVec3;
use marlin_renderer::ViewTarget;
use marlin_world::Transform;

/// A viewport whose camera flies forward on W while the right button is held (the engine's
/// spectator bindings, cut down).
fn viewport(index: u32) -> SceneViewport {
    let actions = SpectatorActions {
        context: Uuid::from_u128(10),
        movement: Uuid::from_u128(11),
        look: Uuid::from_u128(12),
        rise: Uuid::from_u128(13),
        boost: Uuid::from_u128(14),
    };
    let enable = Uuid::from_u128(15);
    let mappings = vec![
        Mapping {
            action: Link::new(actions.movement),
            source: InputSource::Key(KeyCode(Key::W)),
            modifiers: vec![Modifier::Swizzle(SwizzleOrder::Yxz)],
            triggers: vec![Trigger::Chord(Link::new(enable))],
        },
        Mapping {
            action: Link::new(enable),
            source: InputSource::Mouse(Mouse::Right),
            modifiers: Vec::new(),
            triggers: Vec::new(),
        },
    ];
    let context = CompiledContext::compile(actions.context, &MappingContext { mappings }, |a| {
        Some(InputAction {
            value: if a == actions.movement {
                ActionValueType::Axis2D
            } else {
                ActionValueType::Bool
            },
            consume_input: a != enable,
        })
    });
    SceneViewport::new(
        index,
        ViewportCamera::new(Arc::new(context), actions, Transform::IDENTITY),
    )
}

fn after(state: &mut InputState, events: &[InputEvent]) {
    state.begin_frame();
    for &e in events {
        state.apply(e);
    }
}

fn env(release: bool) -> ViewportEnv {
    ViewportEnv {
        hovered: true,
        focused: true,
        text_focus: false,
        release,
    }
}

fn right(down: bool) -> InputEvent {
    InputEvent::MouseButton {
        button: MouseButton::Right,
        down,
    }
}

/// W and the right button, as the frame's input.
fn flying() -> InputFrame {
    InputFrame::empty()
        .with_key(Key::W)
        .with_mouse(Mouse::Right)
}

#[test]
fn editing_flies_the_viewport_camera_and_the_world_gets_nothing() {
    let mut state = InputState::default();
    let mut v = viewport(0);
    let settings = InputSettings::default();

    // Holding the right button over it captures: the camera flies, the world gets nothing.
    after(&mut state, &[right(true)]);
    for _ in 0..5 {
        let out = v.route(&state, flying(), env(false), settings, 0.1);
        assert_eq!(v.input_state(), ViewportState::Captured);
        assert_eq!(out.cursor, CursorMode::Locked);
        assert_eq!(out.world, InputFrame::empty());
        after(&mut state, &[]);
    }
    // Looking down -Z at 10 m/s for half a second.
    let at = v.camera.translation;
    assert!(at.abs_diff_eq(DVec3::new(0.0, 0.0, -5.0), 1e-4), "{at:?}");

    // Let go: the camera stops.
    after(&mut state, &[right(false)]);
    let out = v.route(
        &state,
        InputFrame::empty().with_key(Key::W),
        env(false),
        settings,
        0.1,
    );
    assert_eq!(v.input_state(), ViewportState::Focused);
    assert_eq!(out.cursor, CursorMode::Visible);
    assert!(v.camera.translation.abs_diff_eq(at, 1e-9));
}

#[test]
fn playing_gives_the_world_the_input_until_released() {
    let mut state = InputState::default();
    let mut v = viewport(0);
    let settings = InputSettings::default();
    v.play();
    assert_eq!(v.mode(), ViewportMode::Play);

    // Captured at once: the world gets W, the viewport's camera stays put.
    after(&mut state, &[]);
    let out = v.route(&state, flying(), env(false), settings, 0.1);
    assert_eq!(v.input_state(), ViewportState::GameCapture);
    assert!(out.world.key(Key::W));
    assert_eq!(v.camera.translation, DVec3::ZERO);

    // Shift+F1: the mouse is free and the world gets nothing, while the game goes on.
    let out = v.route(&state, flying(), env(true), settings, 0.1);
    assert_eq!(v.input_state(), ViewportState::Focused);
    assert_eq!(out.cursor, CursorMode::Visible);
    assert_eq!(out.world, InputFrame::empty());

    // Stopping edits again.
    v.stop();
    assert_eq!(v.mode(), ViewportMode::Edit);
    assert_eq!(v.input_state(), ViewportState::Unfocused);
}

#[test]
fn viewports_draw_their_own_views_into_their_own_targets() {
    let (a, b) = (viewport(0), viewport(1));
    assert_ne!(a.view_id(), b.view_id());
    assert_eq!(b.target(), ViewTarget::Offscreen(1));
    // The viewport's camera draws over any camera of the world aimed at the same target.
    let view = a.camera.view(a.view_id(), a.target());
    assert_eq!(view.priority, i32::MAX);
    assert_eq!(view.target, ViewTarget::Offscreen(0));
}
