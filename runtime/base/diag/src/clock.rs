//! The log clock: record timestamps relative to a start tick.

use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

use marlin_pal::time;

/// The start tick; 0 until set. A real start of 0 is stored as 1, a tick's difference.
static START: AtomicU64 = AtomicU64::new(0);

/// Starts the log clock that [`Record::since_start`](crate::Record::since_start) measures from.
/// Call it early in `main`; otherwise the clock starts at the first record. Later calls do nothing.
pub fn init() {
    start(time::ticks());
}

/// The start tick, set to `now` if the clock has not started.
#[inline]
pub(crate) fn start(now: u64) -> u64 {
    let start = START.load(Ordering::Relaxed);
    if start != 0 {
        return start;
    }
    match START.compare_exchange(0, now.max(1), Ordering::Relaxed, Ordering::Relaxed) {
        Ok(_) => now.max(1),
        Err(start) => start,
    }
}

/// Time from the start tick to `ticks`; zero for ticks before it.
pub(crate) fn since_start(ticks: u64) -> Duration {
    let elapsed = ticks.saturating_sub(start(ticks));
    let nanos = u128::from(elapsed) * 1_000_000_000 / u128::from(time::ticks_per_second().max(1));
    Duration::new(
        (nanos / 1_000_000_000).min(u128::from(u64::MAX)) as u64,
        (nanos % 1_000_000_000) as u32,
    )
}
