//! Monotonic clocks, high-resolution timers, sleeping and frame timing.
//!
//! [`Instant`] stores raw PAL ticks, so taking a timestamp is one clock read; conversion to
//! [`Duration`] happens only when instants are compared or offset. [`FrameClock`] and
//! [`FixedStep`] time the game loop: clamped, scaled and pausable frame deltas, and a
//! deterministic fixed-timestep accumulator.
#![cfg_attr(not(test), no_std)]

mod frame;
mod instant;
mod sleep;
mod stopwatch;

pub use core::time::Duration;

pub use frame::{FixedStep, FixedSteps, FrameClock, FrameTime};
pub use instant::{Instant, ticks_per_second};
pub use sleep::{SPIN_THRESHOLD, sleep, sleep_until};
pub use stopwatch::Stopwatch;
