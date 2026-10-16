use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use marlin_sync::{LazyLock, Once, OnceLock};

static ONCE: Once = Once::new();
static CELL: OnceLock<usize> = OnceLock::new();
static LAZY_RUNS: AtomicUsize = AtomicUsize::new(0);
static LAZY: LazyLock<Vec<u32>> = LazyLock::new(|| {
    LAZY_RUNS.fetch_add(1, Ordering::Relaxed);
    vec![1, 2, 3]
});

#[test]
fn once_runs_exactly_once_under_contention() {
    let runs = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let runs = runs.clone();
            thread::spawn(move || {
                ONCE.call_once(|| {
                    // Slow, so the other threads queue up and sleep.
                    thread::sleep(Duration::from_millis(20));
                    runs.fetch_add(1, Ordering::Relaxed);
                });
                // Everyone returns only after the initializer finished.
                assert_eq!(runs.load(Ordering::Relaxed), 1);
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert!(ONCE.is_completed());
}

#[test]
fn once_retries_after_unwinding_initializer() {
    let once = Once::new();
    let r = std::panic::catch_unwind(|| once.call_once(|| panic!("boom")));
    assert!(r.is_err());
    assert!(!once.is_completed());
    let mut ran = false;
    once.call_once(|| ran = true);
    assert!(ran && once.is_completed());
}

#[test]
fn once_lock_get_or_init_race() {
    let inits = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let inits = inits.clone();
            thread::spawn(move || {
                *CELL.get_or_init(|| {
                    inits.fetch_add(1, Ordering::Relaxed);
                    thread::sleep(Duration::from_millis(10));
                    100 + i
                })
            })
        })
        .collect();
    let values: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(inits.load(Ordering::Relaxed), 1);
    assert!(values.iter().all(|&v| v == values[0]));
    assert_eq!(CELL.get(), Some(&values[0]));
}

#[test]
fn once_lock_set_take_drop() {
    let mut cell = OnceLock::new();
    assert!(cell.get().is_none());
    assert_eq!(cell.set(String::from("a")), Ok(()));
    assert_eq!(cell.set(String::from("b")), Err(String::from("b")));
    assert_eq!(cell.get().map(String::as_str), Some("a"));
    assert_eq!(cell.take().as_deref(), Some("a"));
    assert!(cell.get().is_none());
    cell.get_or_init(|| String::from("c"));

    // Dropping a set cell drops its value exactly once.
    let value = Arc::new(());
    let cell = OnceLock::new();
    cell.set(value.clone()).unwrap();
    assert_eq!(Arc::strong_count(&value), 2);
    drop(cell);
    assert_eq!(Arc::strong_count(&value), 1);
}

#[test]
fn lazy_lock_initializes_once() {
    let handles: Vec<_> = (0..4).map(|_| thread::spawn(|| LAZY.len())).collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), 3);
    }
    assert_eq!(*LAZY, [1, 2, 3]);
    assert_eq!(LAZY_RUNS.load(Ordering::Relaxed), 1);
}
