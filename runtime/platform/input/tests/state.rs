use marlin_app::WindowId;
use marlin_input::{
    COMPOSITION_CAPACITY, DeviceId, DeviceKind, GamepadButton, GamepadKind, GamepadReading,
    InputEvent, InputState, Key, MAX_DEVICES, MAX_GAMEPADS, MAX_TOUCHES, MouseButton, PenSample,
    TEXT_CAPACITY, TouchPhase, radial_deadzone,
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
    s.apply(InputEvent::RawMotion {
        device: DeviceId::UNKNOWN,
        x: 3.0,
        y: -1.0,
    });
    s.apply(InputEvent::RawMotion {
        device: DeviceId::UNKNOWN,
        x: 2.0,
        y: -1.0,
    });
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

#[test]
fn composition_persists_until_cleared() {
    let mut s = InputState::new();
    assert!(s.composition().is_none());
    s.set_composition(Some(("にほんご", 6)));
    let c = s.composition().unwrap();
    assert_eq!((c.text, c.cursor), ("にほんご", 6));
    s.begin_frame();
    assert_eq!(s.composition().unwrap().text, "にほんご");
    // The cursor is clamped to the text and to a char boundary.
    s.set_composition(Some(("日本", 4)));
    assert_eq!(s.composition().unwrap().cursor, 3);
    s.set_composition(Some(("ab", 99)));
    assert_eq!(s.composition().unwrap().cursor, 2);
    // Over-long compositions are cut on a char boundary.
    let long = "語".repeat(COMPOSITION_CAPACITY);
    s.set_composition(Some((&long, long.len())));
    let c = s.composition().unwrap();
    assert_eq!(c.text.len(), COMPOSITION_CAPACITY / 3 * 3);
    assert_eq!(c.cursor, c.text.len());
    s.set_composition(None);
    assert!(s.composition().is_none());
}

#[test]
fn text_holds_a_long_ime_commit() {
    let mut s = InputState::new();
    let commit = "長い文章を一度に確定する".repeat(20);
    assert!(commit.len() <= TEXT_CAPACITY);
    for c in commit.chars() {
        s.apply(InputEvent::Text(c));
    }
    assert_eq!(s.text(), commit);
    assert!(!s.text_truncated());
}

fn touch(id: u32, x: f32, phase: TouchPhase) -> InputEvent {
    InputEvent::Touch {
        id,
        window: WindowId::from_raw(1),
        x,
        y: 0.0,
        phase,
    }
}

#[test]
fn touches_live_from_start_to_end() {
    let mut s = InputState::new();
    s.apply(touch(7, 1.0, TouchPhase::Started));
    s.apply(touch(8, 2.0, TouchPhase::Started));
    let t: Vec<_> = s.touches().copied().collect();
    assert_eq!(t.len(), 2);
    assert!(t[0].pressed && !t[0].released && t[0].id == 7);

    s.begin_frame();
    s.apply(touch(7, 5.0, TouchPhase::Moved));
    s.apply(touch(8, 2.0, TouchPhase::Ended));
    let t: Vec<_> = s.touches().copied().collect();
    assert_eq!(t[0].position, [5.0, 0.0]);
    assert!(!t[0].pressed);
    assert!(t[1].released && !t[1].cancelled);

    // Lifted touches are gone the next frame; a tap within one frame is still seen.
    s.begin_frame();
    s.apply(touch(9, 3.0, TouchPhase::Started));
    s.apply(touch(9, 3.0, TouchPhase::Ended));
    let t: Vec<_> = s.touches().copied().collect();
    assert_eq!(t.len(), 2);
    assert!(t[1].id == 9 && t[1].pressed && t[1].released);

    s.begin_frame();
    s.apply(touch(7, 5.0, TouchPhase::Cancelled));
    assert!(s.touches().next().unwrap().cancelled);
    s.begin_frame();
    assert_eq!(s.touches().count(), 0);
    // Ends for unknown ids are ignored.
    s.apply(touch(1, 0.0, TouchPhase::Ended));
    assert_eq!(s.touches().count(), 0);
}

#[test]
fn touches_beyond_capacity_are_ignored() {
    let mut s = InputState::new();
    for id in 0..MAX_TOUCHES as u32 + 3 {
        s.apply(touch(id, 0.0, TouchPhase::Started));
    }
    assert_eq!(s.touches().count(), MAX_TOUCHES);
    s.apply(InputEvent::FocusLost);
    assert!(s.touches().all(|t| t.released && t.cancelled));
}

fn pen(contact: bool, pressure: f32) -> InputEvent {
    InputEvent::Pen(PenSample {
        window: WindowId::from_raw(1),
        position: [10.0, 20.0],
        pressure,
        tilt: [15.0, -5.0],
        twist: 90.0,
        contact,
        barrel: false,
        eraser: false,
    })
}

#[test]
fn pen_hover_contact_and_leave() {
    let mut s = InputState::new();
    assert!(!s.pen().in_range());
    s.apply(pen(false, 0.0));
    assert!(s.pen().in_range() && !s.pen().down() && !s.pen().pressed());
    s.apply(pen(true, 0.5));
    assert!(s.pen().down() && s.pen().pressed());
    assert_eq!(s.pen().sample().unwrap().pressure, 0.5);
    assert_eq!(s.pen().sample().unwrap().tilt, [15.0, -5.0]);

    s.begin_frame();
    assert!(s.pen().down() && !s.pen().pressed());
    s.apply(pen(false, 0.0));
    assert!(s.pen().released() && s.pen().in_range());
    s.begin_frame();
    s.apply(pen(true, 1.0));
    s.apply(InputEvent::PenLeft);
    assert!(s.pen().pressed() && s.pen().released() && !s.pen().in_range());
}

#[test]
fn devices_are_told_apart() {
    let (a, b) = (DeviceId::from_raw(1), DeviceId::from_raw(2));
    let mut s = InputState::new();
    s.apply(InputEvent::DeviceKey {
        device: a,
        key: Key::W,
        down: true,
    });
    s.apply(InputEvent::DeviceKey {
        device: b,
        key: Key::UP,
        down: true,
    });
    s.apply(InputEvent::RawMotion {
        device: DeviceId::from_raw(3),
        x: 4.0,
        y: 1.0,
    });
    s.apply(InputEvent::DeviceButton {
        device: DeviceId::from_raw(3),
        button: MouseButton::Right,
        down: true,
    });
    assert_eq!(s.devices().count(), 3);
    let da = s.device(a).unwrap();
    assert_eq!(da.kind(), DeviceKind::Keyboard);
    assert!(da.key_down(Key::W) && da.key_pressed(Key::W) && !da.key_down(Key::UP));
    assert!(s.device(b).unwrap().key_down(Key::UP));
    let mouse = s.device(DeviceId::from_raw(3)).unwrap();
    assert_eq!(mouse.kind(), DeviceKind::Mouse);
    assert_eq!(mouse.delta(), [4.0, 1.0]);
    assert!(mouse.button_down(MouseButton::Right) && mouse.button_pressed(MouseButton::Right));
    // Device events do not touch the combined key state; raw motion adds to both.
    assert!(!s.key_down(Key::W));
    assert_eq!(s.mouse().delta(), [4.0, 1.0]);

    s.begin_frame();
    s.apply(InputEvent::DeviceKey {
        device: a,
        key: Key::W,
        down: false,
    });
    let da = s.device(a).unwrap();
    assert!(!da.key_down(Key::W) && da.key_released(Key::W) && !da.key_pressed(Key::W));
    assert_eq!(s.device(DeviceId::from_raw(3)).unwrap().delta(), [0.0, 0.0]);

    s.apply(InputEvent::FocusLost);
    assert!(s.device(b).unwrap().key_released(Key::UP));
    let mouse = s.device(DeviceId::from_raw(3)).unwrap();
    assert!(!mouse.button_down(MouseButton::Right) && mouse.button_released(MouseButton::Right));

    s.apply(InputEvent::DeviceRemoved(a));
    assert!(s.device(a).is_none());
    assert_eq!(s.devices().count(), 2);
}

#[test]
fn device_slots_are_bounded() {
    let mut s = InputState::new();
    for i in 0..MAX_DEVICES as u32 + 4 {
        s.apply(InputEvent::RawMotion {
            device: DeviceId::from_raw(i + 1),
            x: 1.0,
            y: 0.0,
        });
    }
    assert_eq!(s.devices().count(), MAX_DEVICES);
    // Every device's motion still reaches the combined delta.
    assert_eq!(s.mouse().delta(), [(MAX_DEVICES + 4) as f32, 0.0]);
}

#[test]
fn gamepad_kind_and_new_buttons() {
    let mut s = InputState::new();
    let reading = GamepadReading {
        buttons: GamepadButton::Home.bit() | GamepadButton::Touchpad.bit(),
        kind: GamepadKind::DualSense,
        ..GamepadReading::default()
    };
    s.set_gamepad(MAX_GAMEPADS - 1, Some(&reading));
    let pad = s.gamepad(MAX_GAMEPADS - 1);
    assert_eq!(pad.kind(), GamepadKind::DualSense);
    assert!(pad.pressed(GamepadButton::Home) && pad.down(GamepadButton::Touchpad));
    assert!(!pad.down(GamepadButton::Misc));
}
