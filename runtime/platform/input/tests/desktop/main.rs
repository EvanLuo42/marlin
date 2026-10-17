//! marlin-input with a real app, windows and OS input (desktop tier,
//! docs/design/0014-testing.md §5): it needs an interactive desktop session. Some tests inject
//! touch, mouse and keyboard input into a topmost test window while it has the foreground. Run
//! alone with `cargo test -p marlin-input --test desktop`; the gamepad test is manual
//! (`-- --ignored`, with a gamepad connected).
//!
//! Every test that creates windows holds `marlin_test_support::one_at_a_time()`: two topmost
//! test windows taking the foreground at once get each other's injected input, though each test
//! passes alone.

// Windows has the only input backend so far.
#[cfg(marlin_platform = "windows")]
mod gamepad;
#[cfg(marlin_platform = "windows")]
mod input;
#[cfg(marlin_platform = "windows")]
#[path = "platform/windows.rs"]
mod windows;
