//! Clocks and timers: monotonic instants, the wall clock and calendar dates, sleeping, frame
//! timing and the CPU's timestamp counter.
//!
//! [`Instant`] stores raw PAL ticks, so taking a timestamp is one clock read; conversion to
//! [`Duration`] happens only when instants are compared or offset. [`FrameClock`] and
//! [`FixedStep`] time the game loop: clamped, scaled and pausable frame deltas, and a
//! deterministic fixed-timestep accumulator. [`SystemTime`] and [`DateTime`] are for timestamps
//! people read. [`sleep`] uses high-resolution waitable timers on Windows (100 ns) and
//! `nanosleep` elsewhere; [`sleep_until`] adds a spin tail for frame pacing.
#![cfg_attr(not(test), no_std)]

mod frame;
mod instant;
mod sleep;
mod stopwatch;
mod system;

pub use core::time::Duration;

pub use frame::{FixedStep, FixedSteps, FrameClock, FrameTime};
pub use instant::{Instant, ticks_per_second};
pub use sleep::{SPIN_THRESHOLD, sleep, sleep_until};
pub use stopwatch::Stopwatch;
pub use system::{DateTime, SystemTime};

/// The CPU's timestamp counter (`rdtsc`, `cntvct_el0`): cheaper than [`Instant::now`], for
/// profilers. Its frequency is not reported; calibrate against [`Instant`].
#[inline]
pub fn cycles() -> u64 {
    marlin_pal::time::cycles()
}
