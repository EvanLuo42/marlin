//! Monotonic clocks, high-resolution timers and sleeping.
//!
//! [`Instant`] stores raw PAL ticks, so taking a timestamp is one clock read; conversion to
//! [`Duration`] happens only when instants are compared or offset.
#![cfg_attr(not(test), no_std)]

mod instant;
mod sleep;
mod stopwatch;

pub use core::time::Duration;

pub use instant::{Instant, ticks_per_second};
pub use sleep::{SPIN_THRESHOLD, sleep, sleep_until};
pub use stopwatch::Stopwatch;
