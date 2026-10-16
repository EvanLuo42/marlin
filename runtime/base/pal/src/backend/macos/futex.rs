//! `__ulock_wait`/`__ulock_wake`: private but long-stable libSystem API (macOS 10.12+), also used by
//! std and libc++.

use core::ffi::{c_int, c_void};
use core::sync::atomic::AtomicU32;
use core::time::Duration;

use super::Backend;
use crate::contract::Futex;

unsafe extern "C" {
    fn __ulock_wait(operation: u32, addr: *mut c_void, value: u64, timeout_us: u32) -> c_int;
    fn __ulock_wake(operation: u32, addr: *mut c_void, wake_value: u64) -> c_int;
}

const UL_COMPARE_AND_WAIT: u32 = 1;
const ULF_WAKE_ALL: u32 = 0x0000_0100;
/// Return `-errno` instead of setting errno.
const ULF_NO_ERRNO: u32 = 0x0100_0000;

impl Futex for Backend {
    fn wait(addr: &AtomicU32, expected: u32, timeout: Option<Duration>) -> bool {
        // Microseconds; 0 means forever, so round tiny timeouts up to 1. Timeouts above u32::MAX µs
        // (~71 min) are capped, and hitting the cap is reported as a spurious wakeup.
        let (timeout_us, capped) = match timeout {
            None => (0, false),
            Some(t) => {
                let us = t.as_micros().max(1);
                (
                    us.min(u128::from(u32::MAX)) as u32,
                    us > u128::from(u32::MAX),
                )
            }
        };
        // SAFETY: `addr` is a valid aligned u32.
        let r = unsafe {
            __ulock_wait(
                UL_COMPARE_AND_WAIT | ULF_NO_ERRNO,
                addr.as_ptr().cast(),
                expected.into(),
                timeout_us,
            )
        };
        r >= 0 || r != -libc::ETIMEDOUT || capped
    }

    fn wake_one(addr: &AtomicU32) {
        // SAFETY: `addr` is a valid aligned u32.
        unsafe { __ulock_wake(UL_COMPARE_AND_WAIT | ULF_NO_ERRNO, addr.as_ptr().cast(), 0) };
    }

    fn wake_all(addr: &AtomicU32) {
        // SAFETY: as above.
        unsafe {
            __ulock_wake(
                UL_COMPARE_AND_WAIT | ULF_WAKE_ALL | ULF_NO_ERRNO,
                addr.as_ptr().cast(),
                0,
            )
        };
    }
}
