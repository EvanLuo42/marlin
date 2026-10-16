use super::Backend;
use crate::contract::Time;

// Neither clock advances while the system is suspended (CLOCK_MONOTONIC on macOS does).
#[cfg(target_vendor = "apple")]
const CLOCK: libc::clockid_t = libc::CLOCK_UPTIME_RAW;
#[cfg(not(target_vendor = "apple"))]
const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;

impl Time for Backend {
    #[inline]
    fn ticks() -> u64 {
        // SAFETY: `timespec` is plain data; all-zero is valid.
        let mut ts: libc::timespec = unsafe { core::mem::zeroed() };
        // SAFETY: `ts` is a valid out pointer. Cannot fail for a supported clock.
        unsafe { libc::clock_gettime(CLOCK, &mut ts) };
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }

    #[inline]
    fn ticks_per_second() -> u64 {
        1_000_000_000
    }
}
