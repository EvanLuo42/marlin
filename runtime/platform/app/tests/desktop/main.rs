//! marlin-app with real windows and the real message pump (desktop tier,
//! docs/design/0014-testing.md §5): it needs an interactive desktop session, and some windows
//! briefly appear on screen. Run alone with `cargo test -p marlin-app --test desktop`; the
//! clipboard test is manual (`-- --ignored`).
//!
//! Tests run in parallel, each with its own `App` on its own thread. Only the tests that activate
//! a window (show it, restore it, or take the foreground) hold
//! `marlin_test_support::one_at_a_time()`: the foreground window is shared by the whole session,
//! and when another thread changes it while a window is shown, Windows skips that window's
//! WM_WINDOWPOSCHANGED. Hidden windows never activate, so the other tests need no lock.

use marlin_app::WindowDesc;

// Windows has the only app backend so far.
#[cfg(marlin_platform = "windows")]
mod app;
#[cfg(marlin_platform = "windows")]
mod window;
#[cfg(marlin_platform = "windows")]
#[path = "platform/windows.rs"]
mod windows;

/// A hidden 320 × 200 window.
fn hidden(title: &str) -> WindowDesc<'_> {
    WindowDesc {
        title,
        width: 320,
        height: 200,
        visible: false,
        ..WindowDesc::default()
    }
}
