//! Logging, assertions, debugger breaks, stack traces and crash dumps.
//!
//! - Log macros [`error!`], [`warn!`], [`info!`], [`debug!`], [`trace!`] and [`log!`]. Levels above
//!   [`STATIC_MAX_LEVEL`] (set by `marlin_build`) generate no code; the rest pass the run-time
//!   filter ([`set_max_level`]) and go to every registered [`Sink`].
//! - The log path never allocates: records carry `fmt::Arguments`, the registry is a fixed array
//!   of `&'static dyn Sink`, and [`DebugOutput`] formats into a stack buffer.
//! - [`marlin_assert!`] / [`marlin_verify!`] log, break into an attached debugger, then panic.
//! - Errors are logged once, where they are handled: [`chain`] displays one with its sources;
//!   [`fatal!`] and [`OrFatal`] report an unrecoverable one and panic. See [`report`].
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod buf;
pub mod debugger;
mod level;
mod macros;
pub mod report;
mod sink;

pub use buf::FixedBuf;
pub use level::{Level, LevelFilter, STATIC_MAX_LEVEL, enabled, max_level, set_max_level};
pub use report::{Chain, OrFatal, chain, take_fatal_report};
pub use sink::{
    DebugOutput, MAX_SINKS, Record, RegistryFull, Sink, dispatch, flush, register_sink,
};

/// Whether [`marlin_assert!`] checks its condition: everywhere but shipping builds.
pub const ASSERTIONS_ENABLED: bool = !cfg!(marlin_build = "shipping");

#[doc(hidden)]
pub mod __private {
    use core::fmt;

    use crate::{Level, Record};

    #[cold]
    #[inline(never)]
    pub fn log(level: Level, target: &str, file: &str, line: u32, args: fmt::Arguments<'_>) {
        let thread_id = marlin_pal::thread::current_id();
        crate::dispatch(&Record::new(level, target, file, line, thread_id, args));
    }

    /// Out of line so a check costs only a compare and a call at the call site.
    #[cold]
    #[inline(never)]
    #[track_caller]
    pub fn assert_failed(cond: &str, target: &str, msg: Option<fmt::Arguments<'_>>) -> ! {
        match msg {
            Some(msg) => fatal(target, format_args!("assertion failed: {cond}: {msg}")),
            None => fatal(target, format_args!("assertion failed: {cond}")),
        }
    }

    #[track_caller]
    pub fn fatal(target: &str, args: fmt::Arguments<'_>) -> ! {
        crate::report::fatal(target, args)
    }
}
