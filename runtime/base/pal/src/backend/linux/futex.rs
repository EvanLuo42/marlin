use core::ptr;
use core::sync::atomic::AtomicU32;
use core::time::Duration;

use super::Backend;
use crate::backend::unix::errno;
use crate::contract::Futex;

impl Futex for Backend {
    fn wait(addr: &AtomicU32, expected: u32, timeout: Option<Duration>) -> bool {
        // Relative timeout, measured on CLOCK_MONOTONIC.
        let ts = timeout.map(|t| {
            // SAFETY: `timespec` is plain data; all-zero is valid.
            let mut ts: libc::timespec = unsafe { core::mem::zeroed() };
            ts.tv_sec = t.as_secs().try_into().unwrap_or(libc::time_t::MAX);
            ts.tv_nsec = t.subsec_nanos() as _;
            ts
        });
        let ts_ptr = ts
            .as_ref()
            .map_or(ptr::null(), |ts| ts as *const libc::timespec);
        // SAFETY: `addr` is a valid aligned u32; `ts_ptr` is null or points to `ts`.
        let r = unsafe {
            libc::syscall(
                libc::SYS_futex,
                addr.as_ptr(),
                libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
                expected,
                ts_ptr,
            )
        };
        r == 0 || errno() != libc::ETIMEDOUT
    }

    fn wake_one(addr: &AtomicU32) {
        // SAFETY: `addr` is a valid aligned u32.
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                addr.as_ptr(),
                libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                1,
            )
        };
    }

    fn wake_all(addr: &AtomicU32) {
        // SAFETY: as above.
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                addr.as_ptr(),
                libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                i32::MAX,
            )
        };
    }
}
