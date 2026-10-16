//! Error reporting: source chains and fatal errors.
//!
//! Errors are values: nothing logs where one is created or propagated with `?`. The code that
//! decides what to do about it (retry, fall back, give up, exit) logs it once, with [`chain`] so
//! the causes are included. When nothing can be done, [`fatal!`](crate::fatal) or
//! [`OrFatal::or_fatal`] logs it and panics, which in the game ends in `marlin_rt::panic::report`.

use core::error::Error;
use core::fmt;
use core::panic::Location;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{Level, Record};

/// Displays an error followed by its sources: `outer: inner: root`. Allocation-free.
#[derive(Clone, Copy, Debug)]
pub struct Chain<'a>(&'a (dyn Error + 'a));

/// Wraps `error` to display its whole source chain.
///
/// ```ignore
/// marlin_diag::warn!("no hardware adapter ({}); using WARP", marlin_diag::chain(&e));
/// ```
#[inline]
pub fn chain<'a>(error: &'a (dyn Error + 'a)) -> Chain<'a> {
    Chain(error)
}

impl fmt::Display for Chain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)?;
        let mut source = self.0.source();
        while let Some(error) = source {
            write!(f, ": {error}")?;
            source = error.source();
        }
        Ok(())
    }
}

/// Ends an unrecoverable `Result` in a fatal error.
pub trait OrFatal<T> {
    /// The value, or a [`fatal!`](crate::fatal) error `"{context}: {chain}"`. Unlike `expect`,
    /// prints the error's `Display` chain rather than its `Debug`.
    fn or_fatal(self, context: &str) -> T;
}

impl<T, E: Error> OrFatal<T> for Result<T, E> {
    #[inline]
    #[track_caller]
    fn or_fatal(self, context: &str) -> T {
        match self {
            Ok(value) => value,
            Err(error) => fatal_error(context, &error),
        }
    }
}

#[cold]
#[inline(never)]
#[track_caller]
fn fatal_error(context: &str, error: &dyn Error) -> ! {
    fatal("fatal", format_args!("{context}: {}", chain(error)))
}

/// Threads that logged their fatal error and are now panicking; 0 is a free slot (no OS thread
/// has id 0). Several threads can fail at once. If every slot is taken the mark is dropped, and
/// the panic handler logs the failure a second time.
static REPORTED: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

/// Logs at `Error` (ignoring the run-time filter), flushes, breaks into an attached debugger, then
/// panics with `args`.
#[cold]
#[inline(never)]
#[track_caller]
pub(crate) fn fatal(target: &str, args: fmt::Arguments<'_>) -> ! {
    let location = Location::caller();
    let thread_id = marlin_pal::thread::current_id();
    crate::dispatch(&Record::new(
        Level::Error,
        target,
        location.file(),
        location.line(),
        thread_id,
        format_args!("{args} at {location}"),
    ));
    crate::flush();
    // Only this thread reads its own mark, so Relaxed suffices.
    let _ = REPORTED.iter().find(|slot| {
        slot.compare_exchange(0, thread_id, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    });
    crate::debugger::break_if_present();
    panic!("{args}")
}

/// Whether the current thread's panic comes from [`fatal!`](crate::fatal), [`OrFatal`] or a failed
/// assertion, which already logged it and broke into the debugger. Clears the mark.
///
/// For the panic handler, so each failure is reported once. Under std nothing calls it, and a
/// thread that catches its fatal panic keeps the mark until it is taken.
pub fn take_fatal_report() -> bool {
    let thread_id = marlin_pal::thread::current_id();
    REPORTED.iter().any(|slot| {
        slot.compare_exchange(thread_id, 0, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    })
}
