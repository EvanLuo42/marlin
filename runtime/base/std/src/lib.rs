//! Facade over `core`, `alloc` and the L0 base crates; the standard library of Marlin runtime code.
//!
//! Crates above L1 depend on this instead of the individual base crates. `marlin-pal` is
//! deliberately not re-exported: platform capabilities reach upper layers through the wrappers'
//! `ext` modules (`mem::ext`, `diag::ext`, `rt::ext`, `thread::ext`).
#![cfg_attr(not(test), no_std)]

pub extern crate alloc;

pub use ::core;

pub use marlin_diag as diag;
pub use marlin_io as io;
pub use marlin_mem as mem;
pub use marlin_rt as rt;
/// The command line, environment, executable and working directory, exit.
pub use marlin_rt::process;
pub use marlin_sync as sync;
pub use marlin_thread as thread;
pub use marlin_time as time;

/// `#[derive(marlin_std::Error)]` and `#[derive(marlin_std::ErrorKind)]`; see `marlin_error`.
pub use marlin_error::{Error, ErrorKind};

pub mod prelude {
    pub use alloc::borrow::ToOwned;
    pub use alloc::boxed::Box;
    pub use alloc::rc::Rc;
    pub use alloc::string::{String, ToString};
    pub use alloc::sync::Arc;
    pub use alloc::vec::Vec;
    pub use alloc::{format, vec};
}
