mod common;

use std::panic::{self, AssertUnwindSafe};
use std::sync::Once;

use common::Capture;
use marlin_diag::{Level, LevelFilter, marlin_assert, marlin_verify};

static CAPTURE: Capture = Capture::new();

fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| marlin_diag::register_sink(&CAPTURE).unwrap());
}

fn panic_message(f: impl FnOnce()) -> String {
    let payload = panic::catch_unwind(AssertUnwindSafe(f)).unwrap_err();
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap()
}

#[test]
fn passing_checks_are_silent() {
    setup();
    marlin_assert!(1 + 1 == 2);
    marlin_assert!(true, "never {}", "shown");
    marlin_verify!(2 > 1);
    assert!(CAPTURE.take_current_thread().is_empty());
}

#[test]
fn failed_verify_logs_then_panics() {
    setup();
    // Assertion failures bypass the run-time filter.
    marlin_diag::set_max_level(LevelFilter::Off);
    let value = 7;
    let line = line!() + 1;
    let message = panic_message(|| marlin_verify!(value == 3, "value was {value}"));
    assert_eq!(message, "assertion failed: value == 3: value was 7");

    let records = CAPTURE.take_current_thread();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].level, Level::Error);
    assert_eq!(records[0].line, line);
    assert!(
        records[0]
            .message
            .starts_with("assertion failed: value == 3: value was 7 at "),
        "{}",
        records[0].message
    );
}

#[test]
fn failed_assert_without_message() {
    setup();
    let message = panic_message(|| marlin_assert!(Vec::<u8>::new().len() == 1));
    if marlin_diag::ASSERTIONS_ENABLED {
        assert_eq!(message, "assertion failed: Vec::<u8>::new().len() == 1");
        assert_eq!(CAPTURE.take_current_thread().len(), 1);
    }
}

#[test]
fn verify_evaluates_once() {
    setup();
    let mut calls = 0;
    let mut check = || {
        calls += 1;
        true
    };
    marlin_verify!(check());
    assert_eq!(calls, 1);
}

#[test]
fn message_arguments_evaluate_once() {
    setup();
    let mut calls = 0;
    let _ = panic_message(|| {
        marlin_verify!(false, "{}", {
            calls += 1;
            calls
        })
    });
    assert_eq!(calls, 1);
    CAPTURE.take_current_thread();
}
