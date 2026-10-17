//! Keyboard, mouse, gamepad, touch and pen input.
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
//! Cursor visibility, confinement and locking belong to the window: see `marlin_app::Window`'s
//! `set_cursor_grab`. With the cursor locked, [`MouseState::delta`] keeps reporting raw motion.
//!
//! # Platform backends
//!
//! - Windows: keyboard, mouse, IME, touch and pen messages through a `marlin_app::os::windows`
//!   message hook; raw input (`WM_INPUT`) for mouse motion and to tell keyboards and mice apart;
//!   gamepads from XInput (Xbox) and from `hid.dll` with the drivers in [`hid`] (DualSense,
//!   DualShock 4, Switch Pro) plus a heuristic mapping for other HID gamepads.
//! - Linux, macOS: stubs; [`Input::new`] returns `Unsupported`.
//!
//! Everything platform-neutral ([`InputState`], [`InputEvent`], [`Key`], [`hid`]) is usable on
//! its own, for tests, replays and synthetic input.
//!
//! # Extensions
//!
//! [`ext`] holds gamepad features only some platforms have (`gyro`, `adaptive_triggers`,
//! `haptics`), compiled in when `platforms/<name>/platform.toml` lists them.
#![cfg_attr(not(test), no_std)]
#![feature(core_float_math)]

extern crate alloc;

mod backend;
mod contract;
pub mod ext;
mod gamepad;
pub mod hid;
mod input;
mod key;
mod pointer;
mod state;

pub use gamepad::{
    GamepadButton, GamepadKind, GamepadReading, GamepadState, HapticTone, MAX_GAMEPADS, Motion,
    Trigger, TriggerEffect, radial_deadzone,
};
pub use input::{Input, KEY_LABEL_CAPACITY};
pub use key::Key;
pub use marlin_pal::{Error, ErrorKind, Result};
pub use pointer::{MAX_TOUCHES, PenSample, PenState, Touch, TouchPhase};
pub use state::{
    COMPOSITION_CAPACITY, Composition, DeviceId, DeviceKind, DeviceState, InputEvent, InputState,
    MAX_DEVICES, MouseButton, MouseState, TEXT_CAPACITY,
};

const _: () = {
    const fn assert_input<I: contract::Input>() {}
    assert_input::<backend::imp::Input>();
    #[cfg(marlin_ext = "gyro")]
    {
        const fn assert_gyro<I: contract::ext::Gyro>() {}
        assert_gyro::<backend::imp::Input>();
    }
    #[cfg(marlin_ext = "adaptive_triggers")]
    {
        const fn assert_triggers<I: contract::ext::AdaptiveTriggers>() {}
        assert_triggers::<backend::imp::Input>();
    }
    #[cfg(marlin_ext = "haptics")]
    {
        const fn assert_haptics<I: contract::ext::Haptics>() {}
        assert_haptics::<backend::imp::Input>();
    }
};
