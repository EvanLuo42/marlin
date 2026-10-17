//! Clocks: the monotonic tick counter, the wall clock and the CPU's timestamp counter.
//!
//! Raw values only; conversion to durations and calendar dates belongs to `marlin-time`.

use crate::backend::imp::Backend;
use crate::contract::Time;

/// Monotonic tick counter: never goes backwards and ignores wall-clock changes. Does not advance
/// while the system is suspended.
#[inline]
pub fn ticks() -> u64 {
    Backend::ticks()
}

/// Ticks per second; constant for the life of the process.
#[inline]
pub fn ticks_per_second() -> u64 {
    Backend::ticks_per_second()
}

/// Wall-clock time as seconds and nanoseconds since the Unix epoch (1970-01-01 00:00:00 UTC).
///
/// Can jump backwards or forwards when the clock is set; use [`ticks`] to measure intervals.
/// Windows: `GetSystemTimePreciseAsFileTime`; POSIX: `CLOCK_REALTIME`.
#[inline]
pub fn system_time() -> (i64, u32) {
    Backend::system_time()
}

/// Local time's offset from UTC at the instant `unix_seconds`, in seconds east of UTC, daylight
/// saving included. `None` if the OS cannot tell.
#[inline]
pub fn local_offset(unix_seconds: i64) -> Option<i32> {
    Backend::local_offset(unix_seconds)
}

/// The CPU's timestamp counter: `rdtsc` on x86-64, `cntvct_el0` on AArch64. A single unserialized
/// instruction, for profilers that need cheaper timestamps than [`ticks`].
///
/// The frequency is not reported (calibrate against [`ticks`]): the TSC of current x86 CPUs runs
/// at a constant rate near the base clock, and `cntvct_el0` at the generic timer's frequency (24
/// MHz on Apple silicon). Both are synchronized across cores on the CPUs Marlin supports.
#[inline]
pub fn cycles() -> u64 {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: rdtsc is available on every x86-64 CPU.
    let value = unsafe { core::arch::x86_64::_rdtsc() };
    #[cfg(target_arch = "aarch64")]
    let value = {
        let value: u64;
        // SAFETY: cntvct_el0 is readable at EL0 on every supported OS.
        unsafe {
            core::arch::asm!("mrs {}, cntvct_el0", out(reg) value, options(nomem, nostack));
        }
        value
    };
    value
}
