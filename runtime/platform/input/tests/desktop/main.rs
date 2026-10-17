//! marlin-input with a real app, windows and OS input (desktop tier,
//! docs/design/0014-testing.md §5): it needs an interactive desktop session. Some tests inject
//! touch, mouse and keyboard input into a topmost test window while it has the foreground. Run
//! alone with `cargo test -p marlin-input --test desktop`; the gamepad test is manual
//! (`-- --ignored`, with a gamepad connected).
//!
//! Tests run in parallel, each with its own `App` and `Input` on its own thread. Only the tests
//! that inject OS input hold `marlin_test_support::one_at_a_time()`: injected touch and keys go to
//! the screen and the foreground window, both shared by the whole session, so two topmost test
//! windows taking the foreground at once would get each other's input.

// Windows has the only input backend so far.
#[cfg(marlin_platform = "windows")]
mod gamepad;
#[cfg(marlin_platform = "windows")]
mod input;
#[cfg(marlin_platform = "windows")]
#[path = "platform/windows.rs"]
mod windows;
