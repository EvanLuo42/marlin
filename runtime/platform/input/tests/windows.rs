//! The Win32 backend, without windows: hook registration, raw input registration, XInput.
#![cfg(marlin_platform = "windows")]

use marlin_app::App;
use marlin_input::{Input, KEY_LABEL_CAPACITY, Key, MAX_GAMEPADS};

#[test]
fn update_without_windows() {
    let mut app = App::new().unwrap();
    let mut input = Input::new(&app).unwrap();
    for _ in 0..3 {
        app.pump();
        input.update();
        let state = input.state();
        assert_eq!(state.keys_down().count(), 0);
        assert_eq!(state.text(), "");
        assert_eq!(state.mouse().delta(), [0.0, 0.0]);
    }
}

#[test]
fn input_outlives_app() {
    let app = App::new().unwrap();
    let mut input = Input::new(&app).unwrap();
    drop(app);
    input.update();
}

#[test]
fn several_inputs_per_app() {
    let app = App::new().unwrap();
    let inputs: Vec<_> = (0..3).map(|_| Input::new(&app).unwrap()).collect();
    drop(inputs);
    // Dropped inputs free their hook slots.
    let _more: Vec<_> = (0..8).map(|_| Input::new(&app).unwrap()).collect();
}

#[test]
fn key_labels_follow_the_layout() {
    let app = App::new().unwrap();
    let input = Input::new(&app).unwrap();
    let mut buf = [0; KEY_LABEL_CAPACITY];
    // The text depends on the layout and display language, but every letter key has one.
    let label = input.key_label(Key::A, &mut buf).unwrap();
    assert!(!label.is_empty());
    assert!(input.key_label(Key::from_hid(0), &mut buf).is_none());
}

#[test]
fn rumble_on_empty_slot_fails() {
    let app = App::new().unwrap();
    let mut input = Input::new(&app).unwrap();
    input.update();
    for pad in 0..MAX_GAMEPADS {
        if !input.state().gamepad(pad).is_connected() {
            assert!(input.set_rumble(pad, 0.0, 0.0).is_err());
        }
    }
}

/// Posts messages as the OS would deliver them, so translation runs without needing focus.
#[test]
#[ignore = "needs a desktop session"]
fn translates_window_messages() {
    use marlin_app::WindowDesc;
    use marlin_app::os::windows::WindowExt;
    use marlin_input::MouseButton;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        PostMessageW, WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
        WM_MOUSEWHEEL,
    };

    let mut app = App::new().unwrap();
    let window = app
        .create_window(&WindowDesc {
            visible: false,
            ..WindowDesc::default()
        })
        .unwrap();
    let mut input = Input::new(&app).unwrap();
    let post = |msg, wparam: usize, lparam: isize| {
        // SAFETY: posting to our own live window.
        let posted = unsafe { PostMessageW(window.hwnd(), msg, wparam, lparam) };
        assert_ne!(posted, 0);
    };
    let scancode = |code: isize, extended: bool| code << 16 | (extended as isize) << 24 | 1;

    // W (scan code 0x11), Right Ctrl (E0 1D), Up (E0 48).
    post(WM_KEYDOWN, 0x57, scancode(0x11, false));
    post(WM_KEYDOWN, 0x11, scancode(0x1D, true));
    post(WM_KEYDOWN, 0x26, scancode(0x48, true));
    post(WM_KEYUP, 0x26, scancode(0x48, true) | 3 << 30);
    // "a😀": the emoji arrives as a surrogate pair.
    post(WM_CHAR, 'a' as usize, 1);
    post(WM_CHAR, 0xD83D, 1);
    post(WM_CHAR, 0xDE00, 1);
    post(WM_MOUSEMOVE, 0, 30 << 16 | 40);
    post(WM_LBUTTONDOWN, 1, 30 << 16 | 40);
    post(WM_MOUSEWHEEL, (-240i16 as u16 as usize) << 16, 0);
    app.pump();
    input.update();

    let s = input.state();
    assert!(s.key_pressed(Key::W) && s.key_down(Key::W));
    assert!(s.key_down(Key::RIGHT_CTRL) && !s.key_down(Key::LEFT_CTRL));
    assert!(s.key_pressed(Key::UP) && s.key_released(Key::UP) && !s.key_down(Key::UP));
    // TranslateMessage turned the W key-down into a WM_CHAR, queued behind the posted ones.
    assert_eq!(s.text(), "a😀w");
    assert_eq!(s.mouse().position(), Some([40.0, 30.0]));
    assert_eq!(s.mouse().window(), Some(window.id()));
    assert!(s.mouse().pressed(MouseButton::Left));
    assert_eq!(s.mouse().wheel(), [0.0, -2.0]);

    post(WM_LBUTTONUP, 0, 30 << 16 | 40);
    post(WM_KEYUP, 0x57, scancode(0x11, false) | 3 << 30);
    app.pump();
    input.update();
    let s = input.state();
    assert!(s.mouse().released(MouseButton::Left) && !s.mouse().down(MouseButton::Left));
    assert!(s.key_released(Key::W) && !s.key_pressed(Key::W));
    assert_eq!(s.text(), "");
}
