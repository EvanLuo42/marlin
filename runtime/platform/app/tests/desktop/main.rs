//! marlin-app with real windows and the real message pump (desktop tier,
//! docs/design/0014-testing.md §5): it needs an interactive desktop session, and some windows
//! briefly appear on screen. Run alone with `cargo test -p marlin-app --test desktop`; the
//! clipboard test is manual (`-- --ignored`).
//!
//! Every test that creates windows holds `marlin_test_support::one_at_a_time()`: windows going
//! fullscreen, shown, minimized or focused from several threads at once miss each other's
//! events and sizes, though each test passes alone.

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
