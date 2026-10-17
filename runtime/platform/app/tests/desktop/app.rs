//! The app without windows: the pump, threads and displays; the clipboard (manual).

use std::time::Duration;

use marlin_app::{App, Hdr, Lifecycle};

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
        assert!(!d.name().is_empty());
        // Real monitors report a rate; 23.976 Hz to 1000 Hz covers every known one.
        assert!((23_000..=1_000_000).contains(&d.refresh_rate_mhz), "{d:?}");
        assert!(d.sdr_white_nits >= 80.0 || d.hdr != Hdr::Enabled, "{d:?}");
    }
}

/// Replaces the user's clipboard briefly (and restores its text), so manual.
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
