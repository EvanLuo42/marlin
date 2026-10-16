//! Debugger interaction.

use marlin_pal::debug;

#[inline]
pub fn is_present() -> bool {
    debug::is_debugger_present()
}

/// Raises a breakpoint trap. Without a debugger this usually terminates the process; see
/// [`break_if_present`].
#[inline]
pub fn break_point() {
    debug::break_point()
}

/// Breaks into the debugger if one is attached; otherwise does nothing.
#[inline]
pub fn break_if_present() {
    if debug::is_debugger_present() {
        debug::break_point();
    }
}
