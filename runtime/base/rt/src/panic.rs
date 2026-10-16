//! Panic reporting.

use core::panic::PanicInfo;

/// Reports a panic and aborts. Called by the game's `#[panic_handler]`.
pub fn report(info: &PanicInfo<'_>) -> ! {
    // Log through marlin-diag and write a crash dump once those exist.
    let _ = info;
    marlin_pal::process::abort()
}
