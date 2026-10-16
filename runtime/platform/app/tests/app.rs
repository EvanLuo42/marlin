//! The Win32 backend. Windowless tests always run; the window test needs a desktop session
//! (`cargo test -p marlin-app -- --ignored`).
#![cfg(marlin_platform = "windows")]

use std::time::Duration;

use marlin_app::os::windows::WindowExt;
use marlin_app::{App, Event, Surface, WindowDesc, WindowMode};

#[test]
fn pump_without_windows() {
    let mut app = App::new().unwrap();
    assert!(app.pump().is_empty());
    assert!(app.pump_timeout(Some(Duration::from_millis(1))).is_empty());
    assert_eq!(app.dropped_events(), 0);
}

#[test]
fn one_app_per_thread() {
    // The window class is shared; creating apps on several threads must not fail.
    let threads: Vec<_> = (0..4)
        .map(|_| std::thread::spawn(|| App::new().map(drop)))
        .collect();
    for t in threads {
        t.join().unwrap().unwrap();
    }
}

#[test]
#[ignore = "needs a desktop session"]
fn window_lifecycle() {
    let mut app = App::new().unwrap();
    let window = app
        .create_window(&WindowDesc {
            title: "marlin-app test",
            width: 320,
            height: 200,
            visible: false,
            ..WindowDesc::default()
        })
        .unwrap();

    let Surface::Win32 { hwnd, .. } = window.surface() else {
        unreachable!("Win32 window")
    };
    assert_eq!(hwnd.as_ptr(), window.hwnd());
    let scale = window.dpi() as f32 / 96.0;
    assert_eq!(window.scale_factor(), scale);
    let size = window.size();
    assert_eq!(size.width, (320.0 * scale) as u32);
    assert_eq!(size.height, (200.0 * scale) as u32);
    assert_eq!(window.mode(), WindowMode::Windowed);
    window.set_title("renamed — 名前");

    // Borderless fullscreen and back restores the windowed size.
    window.set_mode(WindowMode::BorderlessFullscreen).unwrap();
    assert_eq!(window.mode(), WindowMode::BorderlessFullscreen);
    let events = app.pump();
    assert!(events.iter().any(|e| matches!(e, Event::Resized { .. })));
    window.toggle_fullscreen().unwrap();
    assert_eq!(window.mode(), WindowMode::Windowed);
    app.pump();
    assert_eq!(window.size(), size);

    // Closing asks; the window stays until dropped.
    let id = window.id();
    // SAFETY: posting WM_CLOSE to our own live window.
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
            window.hwnd(),
            windows_sys::Win32::UI::WindowsAndMessaging::WM_CLOSE,
            0,
            0,
        )
    };
    assert!(app.pump().contains(&Event::CloseRequested(id)));
    drop(window);
    app.pump();
    assert_eq!(app.dropped_events(), 0);
}
