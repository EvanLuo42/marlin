//! Remaining-time bookkeeping for timed waits, which re-wait after spurious wakeups.
//!
//! Uses the PAL clock directly: `marlin-time` is a sibling L0 crate, and depending on it would be
//! an unregistered same-layer edge for a few lines of arithmetic.

use core::time::Duration;

use marlin_pal::time;

const NANOS_PER_SEC: u64 = 1_000_000_000;

pub(crate) struct Timeout {
    start: u64,
    timeout: Duration,
}

impl Timeout {
    pub(crate) fn start(timeout: Duration) -> Self {
        Self {
            start: time::ticks(),
            timeout,
        }
    }

    /// Time left, or `None` once the timeout has passed.
    pub(crate) fn remaining(&self) -> Option<Duration> {
        let elapsed = ticks_to_duration(time::ticks().saturating_sub(self.start));
        self.timeout
            .checked_sub(elapsed)
            .filter(|left| !left.is_zero())
    }
}

/// The timeout to pass to `futex::wait`: `Some(None)` to wait without one, `None` once `timeout`
/// has passed.
pub(crate) fn wait_time(timeout: Option<&Timeout>) -> Option<Option<Duration>> {
    match timeout {
        None => Some(None),
        Some(t) => t.remaining().map(Some),
    }
}

/// `ticks / ticks_per_second` split into whole seconds and a remainder, so nothing overflows.
fn ticks_to_duration(ticks: u64) -> Duration {
    let tps = time::ticks_per_second();
    let rem = ticks % tps;
    let nanos = if tps <= u64::MAX / NANOS_PER_SEC {
        rem * NANOS_PER_SEC / tps
    } else {
        (u128::from(rem) * u128::from(NANOS_PER_SEC) / u128::from(tps)) as u64
    };
    Duration::new(ticks / tps, nanos as u32)
}
