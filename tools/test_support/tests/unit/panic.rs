use marlin_test_support::{assert_panics_with, panic_message};

#[test]
fn panic_message_reads_both_payload_kinds() {
    assert_eq!(panic_message(|| panic!("static")), "static");
    assert_eq!(panic_message(|| panic!("formatted {}", 1)), "formatted 1");
    assert_panics_with("formatted", || panic!("formatted {}", 2));
}

#[test]
#[should_panic(expected = "expected a panic")]
fn panic_message_fails_when_nothing_panics() {
    panic_message(|| {});
}
