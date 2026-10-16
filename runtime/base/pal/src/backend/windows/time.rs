use core::sync::atomic::{AtomicU64, Ordering};

use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use super::Backend;
use crate::contract::Time;

// Fixed at boot; 0 means not queried yet.
static FREQUENCY: AtomicU64 = AtomicU64::new(0);

impl Time for Backend {
    #[inline]
    fn ticks() -> u64 {
        let mut ticks = 0;
        // SAFETY: `ticks` is a valid out pointer. Cannot fail on Windows XP and later.
        unsafe { QueryPerformanceCounter(&mut ticks) };
        ticks as u64
    }

    #[inline]
    fn ticks_per_second() -> u64 {
        match FREQUENCY.load(Ordering::Relaxed) {
            0 => {
                let mut frequency = 0;
                // SAFETY: `frequency` is a valid out pointer. Cannot fail on Windows XP and later.
                unsafe { QueryPerformanceFrequency(&mut frequency) };
                FREQUENCY.store(frequency as u64, Ordering::Relaxed);
                frequency as u64
            }
            frequency => frequency,
        }
    }
}
