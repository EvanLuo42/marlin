use std::panic::{AssertUnwindSafe, catch_unwind};

/// What `f` panicked with; panics if it did not. For misuse tests that check the message, and
/// for tests that may [`skip!`](crate::skip) first, where `#[should_panic]` cannot tell a skip
/// from a failure.
pub fn panic_message(f: impl FnOnce()) -> String {
    let payload = catch_unwind(AssertUnwindSafe(f)).expect_err("expected a panic");
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => payload
            .downcast_ref::<&str>()
            .map_or_else(String::new, |s| s.to_string()),
    }
}

/// Asserts that `f` panics with a message containing `expected`.
pub fn assert_panics_with(expected: &str, f: impl FnOnce()) {
    let message = panic_message(f);
    assert!(
        message.contains(expected),
        "panicked with {message:?}, expected it to contain {expected:?}"
    );
}
