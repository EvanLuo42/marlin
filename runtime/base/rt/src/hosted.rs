//! Startup for binaries that link std (the editor and tools): std owns the entry point and panics.
//!
//! This crate stays no_std, so it cannot install std's panic hook itself; the binary does, in two
//! lines, forwarding to [`report_panic`]:
//!
//! ```ignore
//! marlin_rt::hosted::init()?;
//! std::panic::set_hook(Box::new(|info| {
//!     marlin_rt::hosted::report_panic(&info.payload_as_str().unwrap_or("Box<dyn Any>"), info.location())
//! }));
//! ```

use core::fmt;
use core::panic::Location;

use crate::cpu::MissingFeatures;

/// Runs the startup sequence under std (see the [crate docs](crate)). Register
/// [`marlin_mem::MarlinHeap`] as the global allocator first. Fails if the CPU lacks an
/// instruction set the build requires; report it and exit.
pub fn init() -> Result<(), MissingFeatures> {
    crate::startup()
}

/// What a std panic hook calls: logs the panic with its backtrace through `marlin_diag` (unless
/// `fatal!` already did), breaks into an attached debugger and writes a crash dump where
/// supported. Std then unwinds or aborts as configured.
pub fn report_panic(message: &dyn fmt::Display, location: Option<&Location<'_>>) {
    crate::panic::log_panic(message, location);
}
