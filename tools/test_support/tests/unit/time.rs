use std::time::Duration;

use marlin_test_support::{assert_elapsed, panic_message, with_timeout};

#[test]
fn with_timeout_returns_the_value() {
    assert_eq!(with_timeout(Duration::from_secs(10), "add", || 2 + 2), 4);
}

#[test]
fn with_timeout_names_what_hung() {
    let message = panic_message(|| {
        with_timeout(Duration::from_millis(20), "the sleeper", || {
            std::thread::sleep(Duration::from_secs(2));
        })
    });
    assert!(message.contains("the sleeper"), "{message}");
}

#[test]
fn with_timeout_passes_panics_through() {
    let message = panic_message(|| with_timeout(Duration::from_secs(10), "x", || panic!("inner")));
    assert_eq!(message, "inner");
}

#[test]
fn assert_elapsed_checks_both_bounds() {
    assert_elapsed(
        Duration::from_millis(10),
        Some(Duration::from_secs(10)),
        || {
            std::thread::sleep(Duration::from_millis(10));
        },
    );
    let message = panic_message(|| assert_elapsed(Duration::from_secs(5), None, || {}));
    assert!(message.contains("at least"), "{message}");
}
