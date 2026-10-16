use core::time::Duration;

use crate::Instant;

/// Accumulates elapsed time across start/stop intervals.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stopwatch {
    accumulated: Duration,
    started: Option<Instant>,
}

impl Stopwatch {
    /// A stopped stopwatch reading zero.
    #[inline]
    pub const fn new() -> Self {
        Self {
            accumulated: Duration::ZERO,
            started: None,
        }
    }

    /// A running stopwatch reading zero.
    #[inline]
    pub fn start_new() -> Self {
        Self {
            accumulated: Duration::ZERO,
            started: Some(Instant::now()),
        }
    }

    #[inline]
    pub fn is_running(&self) -> bool {
        self.started.is_some()
    }

    /// Resumes counting. No effect if already running.
    #[inline]
    pub fn start(&mut self) {
        if self.started.is_none() {
            self.started = Some(Instant::now());
        }
    }

    /// Pauses counting, keeping the reading. No effect if already stopped.
    #[inline]
    pub fn stop(&mut self) {
        if let Some(started) = self.started.take() {
            self.accumulated += started.elapsed();
        }
    }

    /// Stops and zeroes the reading.
    #[inline]
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Returns the reading and restarts from zero, running. Reads the clock once, so consecutive
    /// laps add up to the total with nothing lost between them.
    #[inline]
    pub fn restart(&mut self) -> Duration {
        let now = Instant::now();
        let elapsed = self.elapsed_at(now);
        self.accumulated = Duration::ZERO;
        self.started = Some(now);
        elapsed
    }

    /// Total time spent running, including the current interval.
    #[inline]
    pub fn elapsed(&self) -> Duration {
        self.elapsed_at(Instant::now())
    }

    fn elapsed_at(&self, now: Instant) -> Duration {
        match self.started {
            Some(started) => self.accumulated + now.duration_since(started),
            None => self.accumulated,
        }
    }
}
