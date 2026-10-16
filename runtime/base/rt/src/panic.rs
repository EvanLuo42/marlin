//! Panic reporting.

use core::fmt;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use marlin_diag::{Level, Record};
use marlin_pal::{debug, process, thread};

static PANICKING: AtomicBool = AtomicBool::new(false);

/// Reports a panic and aborts. Called by the game's `#[panic_handler]`.
///
/// Logs the panic at `Error` through every `marlin_diag` sink, then breaks into an attached
/// debugger. Both are skipped for `marlin_diag::fatal!` and failed assertions, which have done
/// them already. Panicking again while reporting (a sink that panics) aborts immediately.
pub fn report(info: &PanicInfo<'_>) -> ! {
    if PANICKING.swap(true, Ordering::Relaxed) {
        process::abort();
    }

    if !marlin_diag::take_fatal_report() {
        let message = info.message();
        let (file, line) = info.location().map_or(("", 0), |l| (l.file(), l.line()));
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
        match info.location() {
            Some(location) => log(format_args!("panicked at {location}: {message}")),
            None => log(format_args!("panicked: {message}")),
        }
        marlin_diag::flush();

        if debug::is_debugger_present() {
            debug::break_point();
        }
    }
    // Crash dumps go here once the crash_dump extension exists.
    process::abort()
}
