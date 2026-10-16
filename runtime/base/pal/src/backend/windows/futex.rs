use core::sync::atomic::AtomicU32;
use core::time::Duration;

use windows_sys::Win32::Foundation::{ERROR_TIMEOUT, GetLastError};
use windows_sys::Win32::System::Threading::{
    INFINITE, WaitOnAddress, WakeByAddressAll, WakeByAddressSingle,
};

use super::Backend;
use crate::contract::Futex;

impl Futex for Backend {
    fn wait(addr: &AtomicU32, expected: u32, timeout: Option<Duration>) -> bool {
        // Milliseconds, rounded up so short timeouts do not become non-blocking.
        let ms = timeout.map_or(INFINITE, |t| {
            t.as_nanos()
                .div_ceil(1_000_000)
                .min(u128::from(INFINITE - 1)) as u32
        });
        // SAFETY: both pointers are valid for 4 bytes for the duration of the call.
        let woken = unsafe {
            WaitOnAddress(addr.as_ptr().cast(), (&raw const expected).cast(), 4, ms) != 0
        };
        // SAFETY: no preconditions.
        woken || unsafe { GetLastError() } != ERROR_TIMEOUT
    }

    fn wake_one(addr: &AtomicU32) {
        // SAFETY: `addr` is a valid address; waking has no other preconditions.
        unsafe { WakeByAddressSingle(addr.as_ptr().cast()) };
    }

    fn wake_all(addr: &AtomicU32) {
        // SAFETY: as above.
        unsafe { WakeByAddressAll(addr.as_ptr().cast()) };
    }
}
