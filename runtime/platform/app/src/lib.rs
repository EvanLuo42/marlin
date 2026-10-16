//! Application lifecycle, windows and displays, and presentation surfaces.
//!
//! [`App`] owns the main thread's OS message pump. Each frame the game calls [`App::pump`], which
//! drains the OS queue and returns the frame's [`Event`]s from a fixed-size, allocation-free
//! [`EventQueue`]. Nothing here needs `std`: the no_std game drives the pump from its own loop.
//!
//! ```ignore
//! let mut app = App::new()?;
//! let window = app.create_window(&WindowDesc { title: "Game", ..WindowDesc::default() })?;
//! 'main: loop {
//!     for event in app.pump() {
//!         if let Event::CloseRequested(_) = event { break 'main; }
//!     }
//!     // update and render
//! }
//! ```
//!
//! Keyboard, mouse and gamepad state lives in `marlin-input`, which observes the OS messages
//! through [`os`] hooks instead of this crate's event queue: high-rate input (raw mouse motion at
//! 8 kHz) is accumulated, not queued.
//!
//! # Structure
//!
//! The PAL pattern: `contract` traits, public types forwarding to `backend::imp`, and [`os`]
//! escape hatches. Windows has a Win32 backend; Linux and macOS are stubs whose [`App::new`]
//! returns [`ErrorKind::Unsupported`]. Console backends come from `platforms/<name>/app/`.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod app;
mod backend;
mod contract;
mod event;
pub mod os;
mod window;

pub use app::App;
pub use event::{EVENT_CAPACITY, Event, EventQueue, WindowId};
pub use marlin_pal::{Error, ErrorKind, Result};
pub use window::{Size, Surface, Window, WindowDesc, WindowMode};

// The selected backend must implement the whole contract.
const _: () = {
    const fn assert_app<A: contract::App<Window = W>, W: contract::Window>() {}
    assert_app::<backend::imp::App, backend::imp::Window>();
};
