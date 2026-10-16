//! Keyboard, mouse, gamepad and touch input.
//!
//! [`Input`] gathers OS input as the app pumps messages and, once per frame,
//! [`update`](Input::update) latches it into an [`InputState`] snapshot: what is held, plus
//! edges (pressed / released) and deltas since the previous frame. The snapshot stays fixed for
//! the whole frame, so gameplay, UI and camera code read consistent values.
//!
//! ```ignore
//! let mut input = Input::new(&app)?;
//! loop {
//!     app.pump();
//!     input.update();
//!     let state = input.state();
//!     if state.key_pressed(Key::SPACE) { jump(); }
//!     camera.rotate(state.mouse().delta());
//! }
//! ```
//!
//! # Platform backends
//!
//! - Windows: keyboard and mouse messages through a `marlin_app::os::windows` message hook, raw
//!   mouse motion from `WM_INPUT`, gamepads from XInput (loaded at run time; without it no
//!   gamepad ever connects).
//! - Linux, macOS: stubs; [`Input::new`] returns `Unsupported`.
//!
//! Everything platform-neutral ([`InputState`], [`InputEvent`], [`Key`]) is usable on its own, for
//! tests, replays and synthetic input.
#![cfg_attr(not(test), no_std)]
#![feature(core_float_math)]

extern crate alloc;

mod backend;
mod contract;
mod input;
mod key;
mod state;

pub use input::{Input, KEY_LABEL_CAPACITY};
pub use key::Key;
pub use marlin_pal::{Error, ErrorKind, Result};
pub use state::{
    GamepadButton, GamepadReading, GamepadState, InputEvent, InputState, MAX_GAMEPADS, MouseButton,
    MouseState, TEXT_CAPACITY, radial_deadzone,
};

const _: () = {
    const fn assert_input<I: contract::Input>() {}
    assert_input::<backend::imp::Input>();
};
