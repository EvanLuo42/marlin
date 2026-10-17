//! The Win32 backend with windows: message translation, the IME, extensions, and touch, mouse and
//! keyboard input injected through the OS; connected XInput and HID gamepads (manual).

use std::time::{Duration, Instant};

use marlin_app::App;
use marlin_input::{Input, Key, MAX_GAMEPADS};
use marlin_test_support::{one_at_a_time, skip, wait_until};

/// Posts messages as the OS would deliver them, so translation runs without needing focus.
#[test]
fn translates_window_messages() {
    let _desktop = one_at_a_time();
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

#[test]
fn extensions_are_available() {
    use marlin_input::ext::adaptive_triggers::AdaptiveTriggers;
    use marlin_input::ext::gyro::Gyro;
    use marlin_input::ext::haptics::Haptics;
    use marlin_input::{ErrorKind, HapticTone, Trigger, TriggerEffect};

    let app = App::new().unwrap();
    let mut input = Input::new(&app).unwrap();
    input.update();
    let gyro = Gyro::get(&input).expect("Windows reads gamepad motion");
    let triggers = AdaptiveTriggers::get(&input).expect("Windows drives adaptive triggers");
    let haptics = Haptics::get(&input).expect("Windows plays HD rumble");
    for pad in 0..MAX_GAMEPADS {
        let state = *input.state().gamepad(pad);
        if state.is_connected() {
            continue;
        }
        assert_eq!(gyro.motion(&state), None);
        let kind = |r: marlin_input::Result<()>| r.unwrap_err().kind();
        assert_eq!(
            kind(gyro.set_enabled(&mut input, pad, false)),
            ErrorKind::NotFound
        );
        assert_eq!(
            kind(triggers.set_effect(&mut input, pad, Trigger::Left, &TriggerEffect::Off)),
            ErrorKind::NotFound
        );
        assert_eq!(
            kind(haptics.play(&mut input, pad, HapticTone::OFF, HapticTone::OFF)),
            ErrorKind::NotFound
        );
    }
}

fn topmost_window(app: &App, title: &str) -> marlin_app::Window {
    use marlin_app::os::windows::WindowExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SetForegroundWindow, SetWindowPos,
    };
    let window = app
        .create_window(&marlin_app::WindowDesc {
            title,
            width: 400,
            height: 300,
            ..marlin_app::WindowDesc::default()
        })
        .unwrap();
    // SAFETY: our own live window.
    unsafe {
        SetWindowPos(
            window.hwnd(),
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE,
        );
        SetForegroundWindow(window.hwnd());
    }
    window
}

/// Pumps and updates until `done` holds for the latest state; fails the test, naming `what`,
/// after two seconds.
fn pump_until(
    app: &mut App,
    input: &mut Input,
    what: &str,
    mut done: impl FnMut(&marlin_input::InputState) -> bool,
) {
    wait_until(Duration::from_secs(2), what, || {
        app.pump_timeout(Some(Duration::from_millis(10)));
        input.update();
        done(input.state())
    });
}

#[test]
fn text_input_toggles_the_ime() {
    let _desktop = one_at_a_time();
    use marlin_app::Rect;
    use marlin_app::os::windows::WindowExt;
    use windows_sys::Win32::UI::Input::Ime::{ImmGetContext, ImmReleaseContext};

    let app = App::new().unwrap();
    let window = app
        .create_window(&marlin_app::WindowDesc {
            visible: false,
            ..marlin_app::WindowDesc::default()
        })
        .unwrap();
    let mut input = Input::new(&app).unwrap();
    let has_context = || {
        // SAFETY: our own live window; a context obtained is released.
        unsafe {
            let context = ImmGetContext(window.hwnd());
            if !context.is_null() {
                ImmReleaseContext(window.hwnd(), context);
            }
            !context.is_null()
        }
    };
    let field = Rect::new(20, 40, 300, 24);
    input.set_text_input(&window, Some(field)).unwrap();
    assert!(has_context());
    input.set_text_input(&window, None).unwrap();
    assert!(!has_context(), "no IME while text input is off");
    input.set_text_input(&window, Some(field)).unwrap();
    assert!(has_context());
    assert!(input.state().composition().is_none());
}

/// Touch injected as real pointer input (`InjectTouchInput`); the window is topmost, so the
/// contacts land on it. Pen translation is not covered here: synthetic pen devices
/// (`InjectSyntheticPointerInput`) deliver nothing without pen hardware.
#[test]
fn touch_from_injected_contacts() {
    let _desktop = one_at_a_time();
    use std::mem;

    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::Input::Pointer::{
        InitializeTouchInjection, InjectTouchInput, POINTER_FLAG_DOWN, POINTER_FLAG_INCONTACT,
        POINTER_FLAG_INRANGE, POINTER_FLAG_UP, POINTER_FLAG_UPDATE, POINTER_TOUCH_INFO,
        TOUCH_FEEDBACK_NONE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::PT_TOUCH;

    let mut app = App::new().unwrap();
    let window = topmost_window(&app, "touch");
    let mut input = Input::new(&app).unwrap();
    app.pump();
    let origin = window.position();
    // SAFETY: no preconditions.
    let initialized = unsafe { InitializeTouchInjection(1, TOUCH_FEEDBACK_NONE) };
    assert_ne!(initialized, 0);
    let inject = |flags, x: i32, y: i32| {
        // SAFETY: all-zero is a valid POINTER_TOUCH_INFO.
        let mut contact: POINTER_TOUCH_INFO = unsafe { mem::zeroed() };
        contact.pointerInfo.pointerType = PT_TOUCH;
        contact.pointerInfo.pointerFlags = flags;
        contact.pointerInfo.ptPixelLocation = POINT {
            x: origin.x + x,
            y: origin.y + y,
        };
        // SAFETY: one valid contact.
        let ok = unsafe { InjectTouchInput(1, &contact) };
        assert_ne!(ok, 0, "injection failed");
    };

    // Windows lifts an injected contact unless frames keep coming, so keep updating it.
    let held = POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT;
    inject(
        POINTER_FLAG_DOWN | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT,
        50,
        60,
    );
    inject(held, 50, 60);
    pump_until(&mut app, &mut input, "a pressed touch", |s| {
        inject(held, 50, 60);
        s.touches().any(|t| t.pressed)
    });
    let t = *input.state().touches().next().unwrap();
    assert_eq!(t.window, window.id());
    assert_eq!(t.position, [50.0, 60.0]);
    assert!(!t.released);

    pump_until(&mut app, &mut input, "the touch moved to (80, 70)", |s| {
        inject(held, 80, 70);
        s.touches().any(|t| t.position == [80.0, 70.0])
    });
    inject(POINTER_FLAG_UP, 80, 70);
    pump_until(&mut app, &mut input, "a released touch", |s| {
        s.touches().any(|t| t.released && !t.cancelled)
    });
    // Lifted touches are gone the frame after.
    input.update();
    assert_eq!(input.state().touches().count(), 0);
}

/// Raw input from `SendInput`: a relative mouse move and an F24 press, which raw input does not
/// attribute to a device. Injects only while the test window has the foreground, so nothing
/// reaches other applications.
#[test]
fn raw_input_from_injected_devices() {
    let _desktop = one_at_a_time();
    use marlin_app::os::windows::WindowExt;
    use marlin_input::DeviceId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
        KEYEVENTF_SCANCODE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    let mut app = App::new().unwrap();
    let window = topmost_window(&app, "raw input");
    let mut input = Input::new(&app).unwrap();
    app.pump();
    // SAFETY: no preconditions.
    if unsafe { GetForegroundWindow() } != window.hwnd() {
        skip!("the test window did not get the foreground; nothing injected");
    }
    let send = |input: INPUT| {
        // SAFETY: one valid INPUT.
        let sent = unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
        assert_eq!(sent, 1);
    };
    send(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 7,
                dy: -3,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    });
    // F24 (scan code 0x76): no text, no application binds it.
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: 0x76,
                dwFlags: KEYEVENTF_SCANCODE | flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    send(key(0));
    let mut moved = [0.0; 2];
    let mut device_pressed = false;
    pump_until(&mut app, &mut input, "the mouse move and F24 press", |s| {
        let d = s.mouse().delta();
        moved = [moved[0] + d[0], moved[1] + d[1]];
        device_pressed |= s
            .device(DeviceId::UNKNOWN)
            .is_some_and(|d| d.key_pressed(Key::F24));
        moved == [7.0, -3.0] && device_pressed
    });
    assert!(input.state().key_down(Key::F24));
    send(key(KEYEVENTF_KEYUP));
    pump_until(&mut app, &mut input, "the F24 release", |s| {
        s.key_released(Key::F24)
    });
}

/// Reads every connected gamepad, XInput and HID alike. Connect at least one first.
#[test]
#[ignore = "needs a connected gamepad"]
fn connected_gamepads() {
    let _desktop = one_at_a_time();
    use marlin_input::ext::adaptive_triggers::AdaptiveTriggers;
    use marlin_input::ext::gyro::Gyro;
    use marlin_input::ext::haptics::Haptics;
    use marlin_input::{GamepadKind, HapticTone, Trigger, TriggerEffect};

    let mut app = App::new().unwrap();
    let mut input = Input::new(&app).unwrap();
    pump_until(&mut app, &mut input, "a connected gamepad", |s| {
        s.gamepads().iter().any(|p| p.is_connected())
    });
    // Let reports and setup commands flow for a while.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        app.pump_timeout(Some(Duration::from_millis(10)));
        input.update();
    }
    let gyro = Gyro::get(&input).unwrap();
    for pad in 0..MAX_GAMEPADS {
        let state = *input.state().gamepad(pad);
        if !state.is_connected() {
            continue;
        }
        eprintln!("gamepad {pad}: {:?}", state.kind());
        for v in [state.left_stick(), state.right_stick()].concat() {
            assert!((-1.0..=1.0).contains(&v));
        }
        for v in [state.left_trigger(), state.right_trigger()] {
            assert!((0.0..=1.0).contains(&v));
        }
        let rumble = input.set_rumble(pad, 0.0, 0.0);
        match state.kind() {
            GamepadKind::Generic => assert!(rumble.is_err()),
            _ => rumble.unwrap(),
        }
        match state.kind() {
            GamepadKind::DualSense | GamepadKind::DualShock4 | GamepadKind::SwitchPro => {
                let motion = gyro.motion(&state).expect("motion sensors");
                let g = motion.accel.iter().map(|a| a * a).sum::<f32>().sqrt();
                assert!((5.0..15.0).contains(&g), "gravity: {motion:?}");
            }
            _ => assert!(gyro.motion(&state).is_none()),
        }
        if state.kind() == GamepadKind::DualSense {
            let triggers = AdaptiveTriggers::get(&input).unwrap();
            triggers
                .set_effect(&mut input, pad, Trigger::Right, &TriggerEffect::Off)
                .unwrap();
        }
        if state.kind() == GamepadKind::SwitchPro {
            let haptics = Haptics::get(&input).unwrap();
            haptics
                .play(&mut input, pad, HapticTone::OFF, HapticTone::OFF)
                .unwrap();
            haptics.stop(&mut input, pad).unwrap();
        }
    }
    input.update();
}
