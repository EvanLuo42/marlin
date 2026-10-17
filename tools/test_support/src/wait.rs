use std::time::{Duration, Instant};

/// Calls `done` until it returns true, sleeping a millisecond between calls; panics with `what`
/// once `timeout` has passed, so a hang fails the test instead of stalling the run.
pub fn wait_until(timeout: Duration, what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < timeout,
            "timed out after {timeout:?}: {what}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
