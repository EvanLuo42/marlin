//! Contexts evaluated frame by frame on synthetic input.

use std::sync::Arc;

use marlin_asset::{Link, Uuid};
use marlin_input::Key;
use marlin_input_action::{
    ActionValueType, CompiledContext, InputAction, InputFrame, InputSource, InputUser, KeyCode,
    Mapping, MappingContext, Modifier, Mouse, Pad, SwizzleOrder, Trigger,
};
use marlin_math::Vec3;

const MOVE: Uuid = Uuid::from_u128(1);
const LOOK: Uuid = Uuid::from_u128(2);
const LOOK_ENABLE: Uuid = Uuid::from_u128(3);
const JUMP: Uuid = Uuid::from_u128(4);
const QUIT: Uuid = Uuid::from_u128(5);

fn value_type(id: Uuid) -> Option<ActionValueType> {
    Some(match id {
        MOVE | LOOK => ActionValueType::Axis2D,
        _ => ActionValueType::Bool,
    })
}

fn map(
    action: Uuid,
    source: InputSource,
    modifiers: Vec<Modifier>,
    triggers: Vec<Trigger>,
) -> Mapping {
    Mapping {
        action: Link::new(action),
        source,
        modifiers,
        triggers,
    }
}

fn key(name: &str) -> InputSource {
    InputSource::Key(KeyCode(Key::from_name(name).unwrap()))
}

fn context(id: u128, mappings: Vec<Mapping>) -> Arc<CompiledContext> {
    Arc::new(CompiledContext::compile(
        Uuid::from_u128(id),
        &MappingContext { mappings },
        value_type,
    ))
}

/// A spectator's context: WASD and the left stick move, the mouse looks while the right button
/// is held.
fn spectator() -> Arc<CompiledContext> {
    context(
        100,
        vec![
            map(
                MOVE,
                key("W"),
                vec![Modifier::Swizzle(SwizzleOrder::Yxz)],
                vec![],
            ),
            map(
                MOVE,
                key("S"),
                vec![Modifier::Swizzle(SwizzleOrder::Yxz), Modifier::Negate],
                vec![],
            ),
            map(MOVE, key("D"), vec![], vec![]),
            map(MOVE, key("A"), vec![Modifier::Negate], vec![]),
            map(MOVE, InputSource::LeftStick, vec![], vec![]),
            map(
                LOOK,
                InputSource::MouseMotion,
                vec![Modifier::Scale {
                    factor: Vec3::new(0.01, -0.01, 0.0),
                }],
                vec![Trigger::Chord(Link::new(LOOK_ENABLE))],
            ),
            map(
                LOOK_ENABLE,
                InputSource::Mouse(Mouse::Right),
                vec![],
                vec![],
            ),
            map(
                JUMP,
                InputSource::Pad(Pad::South),
                vec![],
                vec![Trigger::Pressed],
            ),
        ],
    )
}

#[test]
fn keys_and_sticks_add_up_into_an_axis() {
    let mut user = InputUser::new();
    user.add_context(spectator(), 0);
    let frame = InputFrame::empty()
        .with_key(Key::W)
        .with_key(Key::A)
        .with_sticks([0.5, 0.0], [0.0; 2]);
    user.evaluate(&frame, 0.016);
    let m = user.action(MOVE);
    assert!(m.triggered && m.started);
    assert_eq!(m.axis2(), [-0.5, 1.0], "forward, left, half right");

    user.evaluate(&InputFrame::empty(), 0.016);
    let m = user.action(MOVE);
    assert!(!m.triggered && m.completed);
    assert_eq!(m.axis2(), [0.0, 0.0]);
}

#[test]
fn chords_gate_and_triggers_fire_on_edges() {
    let mut user = InputUser::new();
    user.add_context(spectator(), 0);
    // Mouse motion alone: no look.
    user.evaluate(&InputFrame::empty().with_motion([10.0, 5.0]), 0.016);
    assert!(!user.action(LOOK).triggered);
    // With the right button: look, scaled.
    user.evaluate(
        &InputFrame::empty()
            .with_motion([10.0, 5.0])
            .with_mouse(Mouse::Right),
        0.016,
    );
    let look = user.action(LOOK);
    assert!(look.triggered);
    assert!((look.value - Vec3::new(0.1, -0.05, 0.0)).length() < 1e-6);

    // Pressed fires once while held.
    let jump = InputFrame::empty().with_pad(Pad::South);
    user.evaluate(&jump, 0.016);
    assert!(user.action(JUMP).triggered);
    user.evaluate(&jump, 0.016);
    assert!(!user.action(JUMP).triggered);
}

#[test]
fn hold_and_tap_time_their_presses() {
    let hold = Uuid::from_u128(10);
    let tap = Uuid::from_u128(11);
    let mut user = InputUser::new();
    user.add_context(
        context(
            200,
            vec![
                map(hold, key("E"), vec![], vec![Trigger::Hold { seconds: 0.5 }]),
                map(tap, key("E"), vec![], vec![Trigger::Tap { seconds: 0.2 }]),
            ],
        ),
        0,
    );
    let e = InputFrame::empty().with_key(Key::E);
    for _ in 0..4 {
        user.evaluate(&e, 0.1);
        assert!(!user.action(hold).triggered);
    }
    user.evaluate(&e, 0.1);
    assert!(user.action(hold).triggered, "held half a second");
    user.evaluate(&InputFrame::empty(), 0.1);
    assert!(!user.action(tap).triggered, "too long for a tap");

    user.evaluate(&e, 0.1);
    user.evaluate(&InputFrame::empty(), 0.1);
    assert!(user.action(tap).triggered, "a short press");
}

#[test]
fn higher_contexts_consume_what_they_fire_with() {
    let menu = context(
        300,
        vec![map(QUIT, key("Escape"), vec![], vec![Trigger::Pressed])],
    );
    let game = context(301, vec![map(JUMP, key("Escape"), vec![], vec![])]);
    let mut user = InputUser::new();
    user.add_context(game.clone(), 0);
    user.add_context(menu, 10);
    let esc = InputFrame::empty().with_key(Key::ESCAPE);
    let consumed = user.evaluate(&esc, 0.016);
    assert!(user.action(QUIT).triggered);
    assert!(!user.action(JUMP).triggered, "the menu took Escape");
    assert_eq!(consumed, [key("Escape")]);
    // Next frame the menu's Pressed does not fire: Escape reaches the game.
    user.evaluate(&esc, 0.016);
    assert!(user.action(JUMP).triggered);

    // The next layer sees a frame without what this user consumed.
    let mut next = esc.clone();
    for s in &consumed {
        next.consume(s);
    }
    assert!(!next.key(Key::ESCAPE));

    // Removing a context stops its actions (they complete once).
    assert!(user.remove_context(game.id));
    user.evaluate(&esc, 0.016);
    let jump = user.action(JUMP);
    assert!(!jump.triggered && jump.completed);
}

#[test]
fn dead_zones_and_delta_time_shape_sticks() {
    let mut user = InputUser::new();
    user.add_context(
        context(
            400,
            vec![map(
                LOOK,
                InputSource::RightStick,
                vec![
                    Modifier::DeadZone {
                        lower: 0.2,
                        upper: 1.0,
                    },
                    Modifier::ScaleByDeltaTime,
                ],
                vec![],
            )],
        ),
        0,
    );
    user.evaluate(&InputFrame::empty().with_sticks([0.0; 2], [0.1, 0.0]), 0.5);
    assert_eq!(
        user.action(LOOK).axis2(),
        [0.0, 0.0],
        "inside the dead zone"
    );
    user.evaluate(&InputFrame::empty().with_sticks([0.0; 2], [0.6, 0.0]), 0.5);
    let [x, y] = user.action(LOOK).axis2();
    assert!(
        (x - 0.25).abs() < 1e-6 && y == 0.0,
        "half way out, times 0.5 s: {x}"
    );
}

#[test]
fn assets_round_trip_through_serial() {
    let action = InputAction {
        value: ActionValueType::Axis2D,
    };
    let back: InputAction =
        marlin_serial::from_bytes(&marlin_serial::to_bytes(&action).unwrap()).unwrap();
    assert_eq!(back, action);
    let context = MappingContext {
        mappings: vec![map(
            LOOK,
            key("Left Shift"),
            vec![Modifier::Scale {
                factor: Vec3::new(2.0, 1.0, 1.0),
            }],
            vec![
                Trigger::Chord(Link::new(LOOK_ENABLE)),
                Trigger::Hold { seconds: 1.0 },
            ],
        )],
    };
    let back: MappingContext =
        marlin_serial::from_bytes(&marlin_serial::to_bytes(&context).unwrap()).unwrap();
    assert_eq!(back, context);
}
