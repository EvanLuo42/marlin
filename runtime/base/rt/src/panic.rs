//! Panic reporting.

use core::fmt;
use core::panic::{Location, PanicInfo};
use core::sync::atomic::{AtomicBool, Ordering};

use marlin_diag::{Backtrace, Level, Record};
use marlin_pal::{debug, process, thread};

static PANICKING: AtomicBool = AtomicBool::new(false);

/// Reports a panic and aborts. Called by the game's `#[panic_handler]`.
///
/// Logs the panic with its backtrace at `Error` through every `marlin_diag` sink, then breaks
/// into an attached debugger. Both are skipped for `marlin_diag::fatal!` and failed assertions,
/// which have done them already. Then writes a crash dump where the platform supports it (see
/// [`crate::crash`]) and aborts. Panicking again while reporting (a sink that panics) aborts
/// immediately.
pub fn report(info: &PanicInfo<'_>) -> ! {
    if PANICKING.swap(true, Ordering::Relaxed) {
        process::abort();
    }
    log_panic(&info.message(), info.location());
    process::abort()
}

/// Logs a panic (unless `fatal!` already did), breaks into the debugger and writes a dump.
/// Shared by the game's panic handler and the hosted panic hook.
pub(crate) fn log_panic(message: &dyn fmt::Display, location: Option<&Location<'_>>) {
    if !marlin_diag::take_fatal_report() {
        // Leave out this function and its caller.
        let backtrace = Backtrace::capture_skip(2);
        let (file, line) = location.map_or(("", 0), |l| (l.file(), l.line()));
        let log = |args: fmt::Arguments<'_>| {
            let record = Record::new(
                Level::Error,
                module_path!(),
                file,
                line,
                thread::current_id(),
                args,
            );
            marlin_diag::dispatch(&record);
        };
        match location {
            Some(location) => log(format_args!(
                "panicked at {location}: {message}\n{backtrace}"
            )),
            None => log(format_args!("panicked: {message}\n{backtrace}")),
        }
        marlin_diag::flush();

        if debug::is_debugger_present() {
            debug::break_point();
        }
    }
    write_dump();
}

fn write_dump() {
    #[cfg(marlin_ext = "crash_dump")]
    if let Some(path) = marlin_diag::ext::crash_dump::write_now() {
        marlin_diag::error!("crash dump written to {}", path.as_str());
        marlin_diag::flush();
    }
}
