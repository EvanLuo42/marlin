//! The Win32 backend without windows: the pump, displays, and the lifecycle as the hidden helper
//! window reports it. System notifications (sleep, session lock, display off) are sent to the real
//! helper window exactly as Windows sends them.
#![cfg(marlin_platform = "windows")]

use std::time::Duration;

use marlin_app::os::windows::AppExt;
use marlin_app::{App, Event, Hdr, Lifecycle};
use windows_sys::Win32::System::Power::POWERBROADCAST_SETTING;
use windows_sys::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, PBT_POWERSETTINGCHANGE, SendMessageW, WM_DISPLAYCHANGE,
    WM_POWERBROADCAST, WM_WTSSESSION_CHANGE, WTS_REMOTE_CONNECT, WTS_REMOTE_DISCONNECT,
    WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};

#[test]
fn pump_without_windows() {
    let mut app = App::new().unwrap();
    assert!(app.pump().is_empty());
    assert!(app.pump_timeout(Some(Duration::from_millis(1))).is_empty());
    assert_eq!(app.dropped_events(), 0);
    assert_eq!(app.lifecycle(), Lifecycle::Running);
}

#[test]
fn one_app_per_thread() {
    // The window classes are shared; creating apps on several threads must not fail.
    let threads: Vec<_> = (0..4)
        .map(|_| std::thread::spawn(|| App::new().map(drop)))
        .collect();
    for t in threads {
        t.join().unwrap().unwrap();
    }
}

fn send(app: &App, msg: u32, wparam: usize, lparam: isize) {
    // SAFETY: sending to the app's live helper window on its own thread; pointer lparams point to
    // live locals.
    unsafe { SendMessageW(app.message_window(), msg, wparam, lparam) };
}

#[test]
fn suspend_and_resume() {
    let mut app = App::new().unwrap();
    send(&app, WM_POWERBROADCAST, PBT_APMSUSPEND as usize, 0);
    // Windows tells every top-level window; reported once.
    send(&app, WM_POWERBROADCAST, PBT_APMSUSPEND as usize, 0);
    assert_eq!(app.pump(), &[Event::Suspended]);
    assert_eq!(app.lifecycle(), Lifecycle::Suspended);

    send(&app, WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC as usize, 0);
    assert_eq!(app.pump(), &[Event::Resumed]);
    assert_eq!(app.lifecycle(), Lifecycle::Running);
}

/// A `PBT_POWERSETTINGCHANGE` for the console display state (0 off, 1 on, 2 dimmed).
fn display_state(app: &App, state: u32) {
    #[repr(C)]
    struct Setting {
        header: POWERBROADCAST_SETTING,
        rest: [u8; 3],
    }
    let mut setting = Setting {
        header: POWERBROADCAST_SETTING {
            PowerSetting: GUID_CONSOLE_DISPLAY_STATE,
            DataLength: 4,
            Data: [0],
        },
        rest: [0; 3],
    };
    let data = state.to_le_bytes();
    setting.header.Data[0] = data[0];
    setting.rest.copy_from_slice(&data[1..]);
    send(
        app,
        WM_POWERBROADCAST,
        PBT_POWERSETTINGCHANGE as usize,
        (&raw const setting) as isize,
    );
}

#[test]
fn constrained_while_locked_disconnected_or_display_off() {
    let mut app = App::new().unwrap();
    send(&app, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK as usize, 0);
    assert_eq!(app.pump(), &[Event::Constrained]);
    assert_eq!(app.lifecycle(), Lifecycle::Constrained);

    // Further reasons while constrained change nothing visible.
    display_state(&app, 0);
    send(
        &app,
        WM_WTSSESSION_CHANGE,
        WTS_REMOTE_DISCONNECT as usize,
        0,
    );
    assert!(app.pump().is_empty());

    // Dimmed counts as on; constrained until the last reason clears.
    display_state(&app, 2);
    send(&app, WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK as usize, 0);
    assert!(app.pump().is_empty());
    assert_eq!(app.lifecycle(), Lifecycle::Constrained);
    send(&app, WM_WTSSESSION_CHANGE, WTS_REMOTE_CONNECT as usize, 0);
    assert_eq!(app.pump(), &[Event::Unconstrained]);
    assert_eq!(app.lifecycle(), Lifecycle::Running);

    // Suspended takes precedence.
    display_state(&app, 0);
    send(&app, WM_POWERBROADCAST, PBT_APMSUSPEND as usize, 0);
    assert_eq!(app.pump(), &[Event::Constrained, Event::Suspended]);
    assert_eq!(app.lifecycle(), Lifecycle::Suspended);
    send(&app, WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC as usize, 0);
    display_state(&app, 1);
    assert_eq!(app.pump(), &[Event::Resumed, Event::Unconstrained]);
}

#[test]
fn display_changes_coalesce() {
    let mut app = App::new().unwrap();
    for _ in 0..3 {
        send(&app, WM_DISPLAYCHANGE, 32, 0);
    }
    assert_eq!(app.pump(), &[Event::DisplaysChanged]);
}

#[test]
fn displays() {
    let app = App::new().unwrap();
    let displays = app.displays();
    assert!(!displays.is_empty(), "needs at least one display");
    assert!(displays[0].primary, "primary first");
    assert_eq!(displays.iter().filter(|d| d.primary).count(), 1);
    assert_eq!(app.primary_display().unwrap().id, displays[0].id);
    // The primary display holds the desktop origin.
    assert_eq!((displays[0].bounds.x, displays[0].bounds.y), (0, 0));
    for d in &displays {
        assert!(d.bounds.width > 0 && d.bounds.height > 0, "{d:?}");
        assert!(d.bounds.contains_rect(&d.work_area), "{d:?}");
        assert!(d.dpi >= 96, "{d:?}");
        assert!(d.device_name().starts_with(r"\\.\DISPLAY"), "{d:?}");
        assert!(!d.name().is_empty());
        // Real monitors report a rate; 23.976 Hz to 1000 Hz covers every known one.
        assert!((23_000..=1_000_000).contains(&d.refresh_rate_mhz), "{d:?}");
        assert!(d.sdr_white_nits >= 80.0 || d.hdr != Hdr::Enabled, "{d:?}");
    }
}

/// Replaces the user's clipboard briefly (and restores its text), so not run by default.
#[test]
#[ignore = "touches the user's clipboard"]
fn clipboard_round_trip() {
    let app = App::new().unwrap();
    let saved = app.clipboard_text().unwrap();
    let text = "marlin — クリップボード 📋";
    app.set_clipboard_text(text).unwrap();
    assert_eq!(app.clipboard_text().unwrap().as_deref(), Some(text));
    app.set_clipboard_text("").unwrap();
    assert_eq!(app.clipboard_text().unwrap().as_deref(), Some(""));
    if let Some(saved) = saved {
        app.set_clipboard_text(&saved).unwrap();
    }
}
