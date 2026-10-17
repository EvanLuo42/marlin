//! Logging, assertions, debugger breaks, stack traces and crash dumps.
//!
//! - Log macros [`error!`], [`warn!`], [`info!`], [`debug!`], [`trace!`] and [`log!`]. Levels above
//!   [`STATIC_MAX_LEVEL`] (set by `marlin_build`) generate no code; the rest pass the run-time
//!   filter ([`set_max_level`], refined per module by [`set_target_level`]) and go to every
//!   registered [`Sink`].
//! - The log path never allocates: records carry `fmt::Arguments`, the registry is a fixed array
//!   of `&'static dyn Sink`, and the built-in sinks format into stack buffers.
//! - Sinks: [`DebugOutput`] (the default), [`ConsoleSink`] (optionally colored), [`FileSink`],
//!   [`RingSink`] (recent text, for crash reports) and [`AsyncSink`] (writes a [`LineSink`] from
//!   a background thread). Records are stamped with a monotonic tick; [`init`] starts the clock.
//! - [`Backtrace`]: allocation-free stack capture, symbolized when displayed; [`ext::crash_dump`]
//!   writes minidumps and reports unhandled crashes where the platform supports it.
//! - [`marlin_assert!`] / [`marlin_verify!`] log, break into an attached debugger, then panic.
//! - Errors are logged once, where they are handled: [`chain`] displays one with its sources;
//!   [`fatal!`] and [`OrFatal`] report an unrecoverable one and panic. See [`report`].
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod async_sink;
pub mod backtrace;
mod buf;
mod clock;
pub mod debugger;
pub mod ext;
mod file;
mod level;
mod lock;
mod macros;
pub mod report;
mod ring;
mod sink;

pub use async_sink::AsyncSink;
pub use backtrace::Backtrace;
pub use buf::FixedBuf;
pub use clock::init;
pub use file::FileSink;
pub use level::{
    Level, LevelFilter, MAX_TARGET_LEVELS, STATIC_MAX_LEVEL, TargetTableFull, clear_target_level,
    enabled, enabled_for, max_level, set_max_level, set_target_level,
};
pub use report::{Chain, OrFatal, chain, take_fatal_report};
pub use ring::RingSink;
pub use sink::{
    ConsoleSink, DebugOutput, LINE_LEN, LineSink, MAX_SINKS, Record, RegistryFull, Sink, dispatch,
    flush, register_sink, unregister_sink,
};

/// Whether [`marlin_assert!`] checks its condition: everywhere but shipping builds.
pub const ASSERTIONS_ENABLED: bool = !cfg!(marlin_build = "shipping");

#[doc(hidden)]
pub mod __private {
    use core::fmt;

    use crate::{Level, Record};

    pub use crate::level::may_log;

    /// Applies the target rules the macros' [`may_log`] check left out.
    #[cold]
    #[inline(never)]
    pub fn log(level: Level, target: &str, file: &str, line: u32, args: fmt::Arguments<'_>) {
        if !crate::enabled_for(level, target) {
            return;
        }
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
