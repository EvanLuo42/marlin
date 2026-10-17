use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use marlin_pal::futex;
use marlin_test_support::assert_waited;

#[test]
fn returns_immediately_when_value_differs() {
    let word = AtomicU32::new(1);
    assert!(futex::wait(&word, 0, None));
}

#[test]
fn times_out() {
    let word = AtomicU32::new(0);
    let timeout = Duration::from_millis(30);
    let start = Instant::now();
    // A spurious wakeup is allowed to return true early; only check timing on timeout.
    if !futex::wait(&word, 0, Some(timeout)) {
        assert_waited(start, timeout);
    }
}

#[test]
fn wake_all_releases_waiters() {
    let word = Arc::new(AtomicU32::new(0));
    let waiters: Vec<_> = (0..4)
        .map(|_| {
            let word = word.clone();
            std::thread::spawn(move || {
                while word.load(Ordering::Acquire) == 0 {
                    futex::wait(&word, 0, None);
                }
            })
        })
        .collect();

    std::thread::sleep(Duration::from_millis(20));
    word.store(1, Ordering::Release);
    futex::wake_all(&word);
    for waiter in waiters {
        waiter.join().unwrap();
    }
}

#[test]
fn wake_one_releases_a_waiter() {
    let word = Arc::new(AtomicU32::new(0));
    let waiter = {
        let word = word.clone();
        std::thread::spawn(move || {
            while word.load(Ordering::Acquire) == 0 {
                futex::wait(&word, 0, None);
            }
        })
    };
    std::thread::sleep(Duration::from_millis(20));
    word.store(1, Ordering::Release);
    futex::wake_one(&word);
    waiter.join().unwrap();
}
