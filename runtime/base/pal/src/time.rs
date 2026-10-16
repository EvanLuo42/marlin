//! Monotonic clock.
//!
//! The raw tick counter and its frequency; conversion to durations belongs to `marlin-time`.

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
