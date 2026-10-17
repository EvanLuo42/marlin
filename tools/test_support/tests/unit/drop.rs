use marlin_test_support::{DropCounter, Tracker};

#[test]
fn drop_counter_counts_tokens_dropped_on_other_threads() {
    let counter = DropCounter::new();
    let tokens: Vec<_> = (0..4).map(|_| counter.token()).collect();
    std::thread::spawn(move || drop(tokens)).join().unwrap();
    assert_eq!(counter.dropped(), 4);
}

#[test]
fn tracker_reports_leaks() {
    let tracker = Tracker::new();
    let a = tracker.make(1);
    let b = a.clone();
    assert_eq!(a, b);
    assert_eq!((tracker.live(), tracker.created()), (2, 2));
    drop(a);
    std::mem::forget(b);
    let message = marlin_test_support::panic_message(|| tracker.assert_all_dropped());
    assert!(message.contains("leaked 1 of 2"), "{message}");
}
