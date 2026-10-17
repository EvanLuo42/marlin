use std::ops::RangeBounds;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Timed waits may end a little early by the OS's clock; [`assert_elapsed`] allows this much.
pub const CLOCK_SLACK: Duration = Duration::from_millis(1);

/// Runs `f` and asserts that it took at least `min` (less [`CLOCK_SLACK`]), and less than `max`
/// when one is given; returns what `f` returned. For checking that a timed wait waited.
pub fn assert_elapsed<T>(min: Duration, max: Option<Duration>, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let value = f();
    assert_waited(start, min);
    if let Some(max) = max {
        let elapsed = start.elapsed();
        assert!(elapsed < max, "took {elapsed:?}, expected under {max:?}");
    }
    value
}

/// Asserts that at least `timeout` (less [`CLOCK_SLACK`]) has passed since `start`.
pub fn assert_waited(start: Instant, timeout: Duration) {
    let elapsed = start.elapsed();
    assert!(
        elapsed + CLOCK_SLACK >= timeout,
        "waited {elapsed:?}, expected at least {timeout:?}"
    );
}

/// Asserts that `elapsed` lies in `range`; for durations measured some other way.
pub fn assert_duration_in(elapsed: Duration, range: impl RangeBounds<Duration> + std::fmt::Debug) {
    assert!(range.contains(&elapsed), "{elapsed:?} is outside {range:?}");
}

/// Runs `f` on a new thread and returns its result; panics with `what` if it has not finished
/// within `timeout`, so a deadlock fails the test instead of hanging the run. A panic in `f` is
/// resumed on the calling thread. The thread of a timed-out `f` is left running.
pub fn with_timeout<T: Send + 'static>(
    timeout: Duration,
    what: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let _ = tx.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(value)) => {
            let _ = handle.join();
            value
        }
        Ok(Err(payload)) => std::panic::resume_unwind(payload),
        Err(_) => panic!("timed out after {timeout:?}: {what}"),
    }
}
