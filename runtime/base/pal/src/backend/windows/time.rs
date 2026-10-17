use core::sync::atomic::{AtomicU64, Ordering};

use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows_sys::Win32::System::Time::{
    FileTimeToSystemTime, SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime,
};

use super::Backend;
use crate::contract::Time;

// Fixed at boot; 0 means not queried yet.
static FREQUENCY: AtomicU64 = AtomicU64::new(0);

/// 100 ns intervals from 1601-01-01 (the FILETIME epoch) to 1970-01-01.
const EPOCH_DIFFERENCE: i64 = 116_444_736_000_000_000;

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

    fn system_time() -> (i64, u32) {
        let mut ft = FILETIME::default();
        // SAFETY: `ft` is a valid out pointer.
        unsafe { GetSystemTimePreciseAsFileTime(&mut ft) };
        let since_unix = from_filetime(ft) - EPOCH_DIFFERENCE;
        (
            since_unix.div_euclid(10_000_000),
            (since_unix.rem_euclid(10_000_000) * 100) as u32,
        )
    }

    fn local_offset(unix_seconds: i64) -> Option<i32> {
        let intervals = unix_seconds
            .checked_mul(10_000_000)?
            .checked_add(EPOCH_DIFFERENCE)?;
        let utc = to_filetime(u64::try_from(intervals).ok()?);
        let mut utc_st = SYSTEMTIME::default();
        let mut local_st = SYSTEMTIME::default();
        let mut local = FILETIME::default();
        // SAFETY: valid in and out pointers. A null time zone means the current one, with the
        // daylight saving rules in force at that date (unlike FileTimeToLocalFileTime, which
        // applies today's).
        let ok = unsafe {
            FileTimeToSystemTime(&utc, &mut utc_st) != 0
                && SystemTimeToTzSpecificLocalTime(core::ptr::null(), &utc_st, &mut local_st) != 0
                && SystemTimeToFileTime(&local_st, &mut local) != 0
        };
        if !ok {
            return None;
        }
        let offset = (from_filetime(local) - from_filetime(utc)) / 10_000_000;
        i32::try_from(offset).ok()
    }
}

fn from_filetime(ft: FILETIME) -> i64 {
    ((u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)) as i64
}

fn to_filetime(intervals: u64) -> FILETIME {
    FILETIME {
        dwLowDateTime: intervals as u32,
        dwHighDateTime: (intervals >> 32) as u32,
    }
}
