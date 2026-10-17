use core::mem;

use super::Backend;
use crate::contract::Time;

// Neither clock advances while the system is suspended (CLOCK_MONOTONIC on macOS does).
#[cfg(target_vendor = "apple")]
const CLOCK: libc::clockid_t = libc::CLOCK_UPTIME_RAW;
#[cfg(not(target_vendor = "apple"))]
const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;

#[inline]
fn now(clock: libc::clockid_t) -> libc::timespec {
    // SAFETY: `timespec` is plain data; all-zero is valid.
    let mut ts: libc::timespec = unsafe { mem::zeroed() };
    // SAFETY: `ts` is a valid out pointer. Cannot fail for a supported clock.
    unsafe { libc::clock_gettime(clock, &mut ts) };
    ts
}

impl Time for Backend {
    #[inline]
    fn ticks() -> u64 {
        let ts = now(CLOCK);
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }

    #[inline]
    fn ticks_per_second() -> u64 {
        1_000_000_000
    }

    fn system_time() -> (i64, u32) {
        let ts = now(libc::CLOCK_REALTIME);
        (ts.tv_sec, ts.tv_nsec as u32)
    }

    fn local_offset(unix_seconds: i64) -> Option<i32> {
        let t = libc::time_t::try_from(unix_seconds).ok()?;
        // SAFETY: `tm` is plain data; all-zero is valid.
        let mut tm: libc::tm = unsafe { mem::zeroed() };
        // SAFETY: both pointers are valid. localtime_r is the thread-safe variant; it loads the
        // time zone on first use, which may allocate inside libc.
        if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return None;
        }
        i32::try_from(tm.tm_gmtoff).ok()
    }
}
