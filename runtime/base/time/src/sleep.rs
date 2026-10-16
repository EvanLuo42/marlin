use core::time::Duration;

use marlin_pal::thread;

use crate::Instant;

/// How much of the wait [`sleep_until`] spends spinning instead of sleeping. Covers the usual
/// wake-up latency of a high-resolution OS timer.
pub const SPIN_THRESHOLD: Duration = Duration::from_millis(1);

/// Sleeps for at least `duration`. Precision is the OS timer's (well under a millisecond with
/// high-resolution timers, up to a scheduler tick otherwise). A zero duration may yield.
#[inline]
pub fn sleep(duration: Duration) {
    thread::sleep(duration);
}

/// Waits until `deadline` with sub-millisecond precision, for frame pacing: sleeps until
/// [`SPIN_THRESHOLD`] before the deadline, then spins. Returns at once if it has passed.
///
/// The spin costs up to [`SPIN_THRESHOLD`] of CPU per call; use [`sleep`] when lateness is fine.
pub fn sleep_until(deadline: Instant) {
    // Loop: the OS timer and our clock may be different sources, so a sleep can end early.
    loop {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return;
        };
        if left <= SPIN_THRESHOLD {
            break;
        }
        thread::sleep(left - SPIN_THRESHOLD);
    }
    while Instant::now() < deadline {
        core::hint::spin_loop();
    }
}
