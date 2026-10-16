//! Panic reporting.

use core::fmt::Write;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use marlin_pal::{debug, process};

static PANICKING: AtomicBool = AtomicBool::new(false);

/// Reports a panic and aborts. Called by the game's `#[panic_handler]`.
///
/// Breaks into an attached debugger first. Panicking again while reporting aborts immediately.
pub fn report(info: &PanicInfo<'_>) -> ! {
    if PANICKING.swap(true, Ordering::Relaxed) {
        process::abort();
    }

    let mut out = debug::OutputBuffer::new();
    let _ = writeln!(out, "marlin: {info}");
    out.flush();

    if debug::is_debugger_present() {
        debug::break_point();
    }
    // Crash dumps go here once the crash_dump extension exists.
    process::abort()
}
