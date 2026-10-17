//! Frame timing for the game loop: [`FrameClock`] measures each frame, [`FixedStep`] turns its
//! variable delta into a whole number of fixed simulation steps.
//!
//! ```ignore
//! let mut clock = FrameClock::new();
//! let mut physics = FixedStep::new(Duration::from_micros(16_667), 8);
//! loop {
//!     let frame = clock.tick();
//!     let steps = physics.advance(frame.scaled_delta);
//!     for _ in 0..steps.steps {
//!         simulate(physics.step());
//!     }
//!     render(steps.alpha);
//! }
//! ```

use core::time::Duration;

use crate::Instant;

const NANOS_PER_SEC: u128 = 1_000_000_000;

/// Saturates at [`Duration::MAX`].
#[inline]
fn nanos_to_duration(nanos: u128) -> Duration {
    match u64::try_from(nanos / NANOS_PER_SEC) {
        Ok(secs) => Duration::new(secs, (nanos % NANOS_PER_SEC) as u32),
        Err(_) => Duration::MAX,
    }
}

/// Rounds a non-negative float to the nearest integer (`core` has no `f64::round`); `as`
/// saturates.
#[inline]
fn round(x: f64) -> u128 {
    (x + 0.5) as u128
}

/// One frame's timing, from [`FrameClock::tick`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameTime {
    /// Index of this frame; the first tick is frame 0.
    pub frame: u64,
    /// Real time since the previous tick; zero on the first.
    pub raw_delta: Duration,
    /// `raw_delta` clamped to the clock's max delta. Ignores time scale and pause.
    pub delta: Duration,
    /// `delta` times the time scale, zero while paused. What the simulation should advance by.
    pub scaled_delta: Duration,
    /// Sum of every `scaled_delta` so far, this one included.
    pub game_time: Duration,
    /// Real time since the first tick, unclamped.
    pub real_time: Duration,
    /// Exponential moving average of `delta`, for display.
    pub smoothed_delta: Duration,
}

impl FrameTime {
    /// Frames per second from `smoothed_delta`; 0 until a frame has taken any time.
    #[inline]
    pub fn fps(&self) -> f64 {
        let nanos = self.smoothed_delta.as_nanos();
        if nanos == 0 {
            0.0
        } else {
            NANOS_PER_SEC as f64 / nanos as f64
        }
    }
}

/// Measures frames: call [`tick`](Self::tick) once per frame.
///
/// Each delta is clamped to [`max_delta`](Self::max_delta), so a breakpoint or a hitch advances
/// the game by at most that much, then multiplied by the [time scale](Self::set_time_scale).
/// Game time sums the scaled deltas as integer nanoseconds, so it does not drift however long the
/// game runs. The smoothed delta is an exponential moving average of the clamped delta:
/// `avg += factor * (delta - avg)`, seeded with the first nonzero delta.
#[derive(Clone, Copy, Debug)]
pub struct FrameClock {
    max_delta: Duration,
    time_scale: f64,
    paused: bool,
    smoothing: f64,
    origin: Option<Instant>,
    last: Option<Instant>,
    frames: u64,
    game_time: Duration,
    /// In nanoseconds; negative until seeded.
    smoothed: f64,
}

impl FrameClock {
    pub const DEFAULT_MAX_DELTA: Duration = Duration::from_millis(250);
    pub const DEFAULT_SMOOTHING: f64 = 0.1;

    /// A clock with the defaults and scale 1. Reads no time: the first tick starts it.
    #[inline]
    pub const fn new() -> Self {
        Self {
            max_delta: Self::DEFAULT_MAX_DELTA,
            time_scale: 1.0,
            paused: false,
            smoothing: Self::DEFAULT_SMOOTHING,
            origin: None,
            last: None,
            frames: 0,
            game_time: Duration::ZERO,
            smoothed: -1.0,
        }
    }

    /// Starts the next frame: measures the time since the previous tick.
    #[inline]
    pub fn tick(&mut self) -> FrameTime {
        self.tick_at(Instant::now())
    }

    /// [`tick`](Self::tick) with the clock read supplied, for tests and replays. An instant earlier
    /// than the previous one counts as zero time.
    pub fn tick_at(&mut self, now: Instant) -> FrameTime {
        let origin = *self.origin.get_or_insert(now);
        let raw_delta = match self.last {
            Some(last) => now.saturating_duration_since(last),
            None => Duration::ZERO,
        };
        self.last = Some(self.last.map_or(now, |last| last.max(now)));

        let delta = raw_delta.min(self.max_delta);
        let scaled_delta = self.scale(delta);
        self.game_time = self.game_time.saturating_add(scaled_delta);

        let nanos = delta.as_nanos() as f64;
        if self.smoothed < 0.0 {
            if nanos > 0.0 {
                self.smoothed = nanos;
            }
        } else {
            self.smoothed += self.smoothing * (nanos - self.smoothed);
        }

        let frame = self.frames;
        self.frames += 1;
        FrameTime {
            frame,
            raw_delta,
            delta,
            scaled_delta,
            game_time: self.game_time,
            real_time: now.saturating_duration_since(origin),
            smoothed_delta: Duration::from_nanos(round(self.smoothed.max(0.0)) as u64),
        }
    }

    fn scale(&self, delta: Duration) -> Duration {
        if self.paused || self.time_scale == 0.0 {
            Duration::ZERO
        } else if self.time_scale == 1.0 {
            delta
        } else {
            // Rounded to whole nanoseconds.
            nanos_to_duration(round(delta.as_nanos() as f64 * self.time_scale))
        }
    }

    /// The longest delta a single frame can report.
    #[inline]
    pub fn max_delta(&self) -> Duration {
        self.max_delta
    }

    /// Defaults to [`DEFAULT_MAX_DELTA`](Self::DEFAULT_MAX_DELTA); [`Duration::MAX`] disables the
    /// clamp.
    #[inline]
    pub fn set_max_delta(&mut self, max: Duration) {
        self.max_delta = max;
    }

    #[inline]
    pub fn time_scale(&self) -> f64 {
        self.time_scale
    }

    /// Multiplies every following delta: 0.5 is slow motion, 0 freezes game time. Independent of
    /// [`pause`](Self::pause), which keeps the scale for [`resume`](Self::resume).
    ///
    /// # Panics
    /// If `scale` is negative or not finite.
    #[inline]
    pub fn set_time_scale(&mut self, scale: f64) {
        assert!(
            scale.is_finite() && scale >= 0.0,
            "time scale must be finite and non-negative, got {scale}"
        );
        self.time_scale = scale;
    }

    /// Stops game time from the next tick on. Real time, the frame counter and the smoothed delta
    /// keep running.
    #[inline]
    pub fn pause(&mut self) {
        self.paused = true;
    }

    #[inline]
    pub fn resume(&mut self) {
        self.paused = false;
    }

    #[inline]
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    #[inline]
    pub fn smoothing(&self) -> f64 {
        self.smoothing
    }

    /// The moving average's weight for each new delta, in `(0, 1]`; 1 disables smoothing. Defaults
    /// to [`DEFAULT_SMOOTHING`](Self::DEFAULT_SMOOTHING), about the last 10 frames.
    ///
    /// # Panics
    /// If `factor` is outside `(0, 1]`.
    #[inline]
    pub fn set_smoothing(&mut self, factor: f64) {
        assert!(
            factor > 0.0 && factor <= 1.0,
            "smoothing factor must be in (0, 1], got {factor}"
        );
        self.smoothing = factor;
    }

    /// Ticks so far.
    #[inline]
    pub fn frame_count(&self) -> u64 {
        self.frames
    }

    /// Scaled time accumulated so far.
    #[inline]
    pub fn game_time(&self) -> Duration {
        self.game_time
    }
}

impl Default for FrameClock {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// What [`FixedStep::advance`] decided for one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FixedSteps {
    /// Fixed updates to run this frame, at most the step's cap.
    pub steps: u32,
    /// How far the leftover time is into the next step, in `[0, 1)`: render
    /// `lerp(previous, current, alpha)`.
    pub alpha: f32,
    /// Whole steps of time thrown away because the cap was hit; zero otherwise.
    pub dropped: Duration,
}

/// A fixed-timestep accumulator: banks each frame's delta and pays it out in whole steps.
///
/// All arithmetic is on integer nanoseconds, so the same deltas always give the same steps. When a
/// frame owes more than `max_steps_per_frame` steps, the excess whole steps are dropped (the game
/// slows down rather than spiralling) and the sub-step remainder is kept.
#[derive(Clone, Copy, Debug)]
pub struct FixedStep {
    step: Duration,
    max_steps_per_frame: u32,
    accumulator: Duration,
}

impl FixedStep {
    /// # Panics
    /// If `step` or `max_steps_per_frame` is zero.
    #[inline]
    pub const fn new(step: Duration, max_steps_per_frame: u32) -> Self {
        assert!(!step.is_zero(), "fixed step must be nonzero");
        assert!(
            max_steps_per_frame > 0,
            "max steps per frame must be nonzero"
        );
        Self {
            step,
            max_steps_per_frame,
            accumulator: Duration::ZERO,
        }
    }

    #[inline]
    pub fn step(&self) -> Duration {
        self.step
    }

    #[inline]
    pub fn max_steps_per_frame(&self) -> u32 {
        self.max_steps_per_frame
    }

    /// Time banked toward the next step; always less than [`step`](Self::step).
    #[inline]
    pub fn accumulated(&self) -> Duration {
        self.accumulator
    }

    /// Empties the accumulator.
    #[inline]
    pub fn reset(&mut self) {
        self.accumulator = Duration::ZERO;
    }

    /// Banks `scaled_delta` (usually [`FrameTime::scaled_delta`]) and returns how many steps to
    /// run this frame.
    pub fn advance(&mut self, scaled_delta: Duration) -> FixedSteps {
        let step = self.step.as_nanos();
        let total = self.accumulator.as_nanos() + scaled_delta.as_nanos();
        let owed = total / step;
        let remainder = total % step;
        let steps = owed.min(u128::from(self.max_steps_per_frame));
        self.accumulator = nanos_to_duration(remainder);
        // Largest f32 below 1, in case the division rounds up.
        const BELOW_ONE: f32 = 1.0 - f32::EPSILON / 2.0;
        FixedSteps {
            steps: steps as u32,
            alpha: ((remainder as f64 / step as f64) as f32).min(BELOW_ONE),
            dropped: nanos_to_duration((owed - steps).saturating_mul(step)),
        }
    }
}
