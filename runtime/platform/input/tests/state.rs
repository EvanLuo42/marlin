use marlin_app::WindowId;
use marlin_input::{
    GamepadButton, GamepadReading, InputEvent, InputState, Key, MouseButton, TEXT_CAPACITY,
    radial_deadzone,
};

fn key(key: Key, down: bool) -> InputEvent {
    InputEvent::Key {
        key,
        down,
        repeat: false,
    }
}

#[test]
fn press_hold_release() {
    let mut s = InputState::new();
    s.apply(key(Key::W, true));
    assert!(s.key_down(Key::W) && s.key_pressed(Key::W) && !s.key_released(Key::W));

    s.begin_frame();
    assert!(s.key_down(Key::W) && !s.key_pressed(Key::W));

    s.begin_frame();
    s.apply(key(Key::W, false));
    assert!(!s.key_down(Key::W) && s.key_released(Key::W));
    assert_eq!(s.keys_down().count(), 0);
}

#[test]
fn tap_within_one_frame_is_not_lost() {
    let mut s = InputState::new();
    s.apply(key(Key::SPACE, true));
    s.apply(key(Key::SPACE, false));
    assert!(s.key_pressed(Key::SPACE));
    assert!(s.key_released(Key::SPACE));
    assert!(!s.key_down(Key::SPACE));
}

#[test]
fn auto_repeat_is_not_a_press() {
    let mut s = InputState::new();
    s.apply(key(Key::LEFT, true));
    s.begin_frame();
    s.apply(InputEvent::Key {
        key: Key::LEFT,
        down: true,
        repeat: true,
    });
    assert!(!s.key_pressed(Key::LEFT));
    assert!(s.key_pressed_or_repeated(Key::LEFT));
    // An unflagged down for a held key counts as a repeat too.
    s.begin_frame();
    s.apply(key(Key::LEFT, true));
    assert!(!s.key_pressed(Key::LEFT) && s.key_pressed_or_repeated(Key::LEFT));
}

#[test]
fn release_without_press_reports_both() {
    // Print Screen on Windows only sends the key-up.
    let mut s = InputState::new();
    s.apply(key(Key::PRINT_SCREEN, false));
    assert!(s.key_pressed(Key::PRINT_SCREEN) && s.key_released(Key::PRINT_SCREEN));
    assert!(!s.key_down(Key::PRINT_SCREEN));
}

#[test]
fn modifiers() {
    let mut s = InputState::new();
    assert!(!s.shift() && !s.ctrl() && !s.alt());
    s.apply(key(Key::RIGHT_SHIFT, true));
    s.apply(key(Key::LEFT_CTRL, true));
    s.apply(key(Key::RIGHT_ALT, true));
    assert!(s.shift() && s.ctrl() && s.alt());
}

#[test]
fn focus_loss_releases_everything() {
    let mut s = InputState::new();
    s.apply(key(Key::A, true));
    s.apply(InputEvent::MouseButton {
        button: MouseButton::Left,
        down: true,
    });
    s.begin_frame();
    s.apply(InputEvent::FocusLost);
    assert!(!s.key_down(Key::A) && s.key_released(Key::A));
    assert!(!s.mouse().down(MouseButton::Left) && s.mouse().released(MouseButton::Left));
}

#[test]
fn text_in_order_without_controls() {
    let mut s = InputState::new();
    for c in ['h', 'é', '\r', '\u{8}', '日', '😀', '\u{7f}'] {
        s.apply(InputEvent::Text(c));
    }
    assert_eq!(s.text(), "hé日😀");
    assert!(!s.text_truncated());
    s.begin_frame();
    assert_eq!(s.text(), "");
}

#[test]
fn text_truncates_on_char_boundary() {
    let mut s = InputState::new();
    // 3-byte chars: the last that fits ends at a multiple of 3.
    for _ in 0..TEXT_CAPACITY {
        s.apply(InputEvent::Text('日'));
    }
    assert!(s.text_truncated());
    assert_eq!(s.text().len(), TEXT_CAPACITY / 3 * 3);
    assert!(s.text().chars().all(|c| c == '日'));
}

#[test]
fn mouse_accumulates_per_frame() {
    let window = WindowId::from_raw(7);
    let mut s = InputState::new();
    assert_eq!(s.mouse().position(), None);
    s.apply(InputEvent::MouseMoved {
        window,
        x: 10.0,
        y: 20.0,
    });
    s.apply(InputEvent::RawMotion { x: 3.0, y: -1.0 });
    s.apply(InputEvent::RawMotion { x: 2.0, y: -1.0 });
    s.apply(InputEvent::Wheel { x: 0.0, y: 1.0 });
    s.apply(InputEvent::Wheel { x: 0.5, y: 1.0 });
    s.apply(InputEvent::MouseButton {
        button: MouseButton::X1,
        down: true,
    });
    let m = s.mouse();
    assert_eq!(m.position(), Some([10.0, 20.0]));
    assert_eq!(m.window(), Some(window));
    assert_eq!(m.delta(), [5.0, -2.0]);
    assert_eq!(m.wheel(), [0.5, 2.0]);
    assert!(m.down(MouseButton::X1) && m.pressed(MouseButton::X1));
    assert!(!m.down(MouseButton::Left));

    s.begin_frame();
    let m = s.mouse();
    assert_eq!(m.position(), Some([10.0, 20.0]), "position persists");
    assert_eq!(m.delta(), [0.0, 0.0]);
    assert_eq!(m.wheel(), [0.0, 0.0]);
    assert!(m.down(MouseButton::X1) && !m.pressed(MouseButton::X1));
}

#[test]
fn gamepad_edges_and_disconnect() {
    let mut s = InputState::new();
    assert!(!s.gamepad(0).is_connected());
    let reading = GamepadReading {
        buttons: GamepadButton::South.bit() | GamepadButton::DPadUp.bit(),
        left_stick: [0.5, -0.25],
        right_trigger: 1.0,
        ..GamepadReading::default()
    };
    s.set_gamepad(1, Some(&reading));
    let pad = s.gamepad(1);
    assert!(pad.is_connected());
    assert!(pad.down(GamepadButton::South) && pad.pressed(GamepadButton::South));
    assert!(pad.pressed(GamepadButton::DPadUp));
    assert_eq!(pad.left_stick(), [0.5, -0.25]);
    assert_eq!(pad.right_trigger(), 1.0);

    s.begin_frame();
    s.set_gamepad(1, Some(&reading));
    assert!(!s.gamepad(1).pressed(GamepadButton::South));

    s.begin_frame();
    s.set_gamepad(1, None);
    let pad = s.gamepad(1);
    assert!(!pad.is_connected());
    assert!(pad.released(GamepadButton::South) && !pad.down(GamepadButton::South));
    assert_eq!(pad.left_stick(), [0.0, 0.0]);
}

#[test]
fn radial_deadzone_rescales() {
    assert_eq!(radial_deadzone([0.1, 0.1], 0.2), [0.0, 0.0]);
    assert_eq!(radial_deadzone([1.0, 0.0], 0.2), [1.0, 0.0]);
    let [x, y] = radial_deadzone([0.6, 0.0], 0.2);
    assert!((x - 0.5).abs() < 1e-6 && y == 0.0);
    // Direction is kept; magnitude clamps to 1.
    let [x, y] = radial_deadzone([1.0, 1.0], 0.2);
    assert!((x - y).abs() < 1e-6 && ((x * x + y * y).sqrt() - 1.0).abs() < 1e-6);
}

#[test]
fn key_names() {
    assert_eq!(Key::A.name(), Some("A"));
    assert_eq!(Key::LEFT_SHIFT.name(), Some("Left Shift"));
    assert_eq!(Key::from_hid(0x04), Key::A);
    assert_eq!(Key::from_hid(0x00).name(), None);
    assert_eq!(Key::from_name("numpad 7"), Some(Key::NUMPAD7));
    assert_eq!(Key::from_name("Space"), Some(Key::SPACE));
    assert_eq!(Key::from_name("nope"), None);
    assert_eq!(format!("{}", Key::ESCAPE), "Escape");
    assert_eq!(format!("{:?}", Key::from_hid(0xFF)), "Key(0xff)");
    // Every named key round-trips through its name.
    for hid in 0..=255u8 {
        let key = Key::from_hid(hid);
        if let Some(name) = key.name() {
            assert_eq!(Key::from_name(name), Some(key), "{name}");
        }
    }
}
