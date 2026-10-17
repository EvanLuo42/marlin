use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use marlin_sync::Semaphore;
use marlin_test_support::assert_waited;

static STATIC_SEMAPHORE: Semaphore = Semaphore::new(0);

#[test]
fn counts_permits() {
    let s = Semaphore::new(2);
    assert!(s.try_acquire());
    s.acquire();
    assert_eq!(s.available_permits(), 0);
    assert!(!s.try_acquire());
    s.release(3);
    assert_eq!(s.available_permits(), 3);
    s.release(0);
    assert_eq!(s.available_permits(), 3);
}

#[test]
fn permit_returns_on_drop_unless_forgotten() {
    let s = Semaphore::new(1);
    {
        let _permit = s.acquire_permit();
        assert!(s.try_acquire_permit().is_none());
        assert!(s.try_acquire_permit_for(Duration::from_millis(5)).is_none());
    }
    assert_eq!(s.available_permits(), 1);
    s.try_acquire_permit().unwrap().forget();
    assert_eq!(s.available_permits(), 0);
    s.release(1);
    assert_eq!(s.available_permits(), 1);
}

#[test]
fn timeout_expires() {
    let s = Semaphore::new(0);
    let timeout = Duration::from_millis(30);
    let start = Instant::now();
    assert!(!s.try_acquire_for(timeout));
    assert_waited(start, timeout);
    assert!(!s.try_acquire_for(Duration::ZERO));
}

#[test]
#[should_panic(expected = "overflow")]
fn release_overflow_panics() {
    let s = Semaphore::new(u32::MAX);
    s.release(1);
}

#[test]
fn release_wakes_sleeper() {
    let waiter = thread::spawn(|| STATIC_SEMAPHORE.try_acquire_for(Duration::from_secs(10)));
    thread::sleep(Duration::from_millis(20));
    STATIC_SEMAPHORE.release(1);
    assert!(waiter.join().unwrap());
}

#[test]
fn release_many_wakes_many() {
    const WAITERS: usize = 6;
    let s = Arc::new(Semaphore::new(0));
    let waiters: Vec<_> = (0..WAITERS)
        .map(|_| {
            let s = s.clone();
            thread::spawn(move || s.acquire())
        })
        .collect();
    thread::sleep(Duration::from_millis(20));
    s.release(WAITERS as u32);
    for w in waiters {
        w.join().unwrap();
    }
    assert_eq!(s.available_permits(), 0);
}

#[test]
fn bounds_concurrency_under_contention() {
    const PERMITS: usize = 3;
    const THREADS: usize = 8;
    const ITERS: usize = 2_000;
    let s = Arc::new(Semaphore::new(PERMITS as u32));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let (s, active, peak) = (s.clone(), active.clone(), peak.clone());
            thread::spawn(move || {
                for i in 0..ITERS {
                    // Mix the guard, timed and raw forms. `None` means a raw permit.
                    let permit = match (t + i) % 3 {
                        0 => Some(s.acquire_permit()),
                        1 => loop {
                            if let Some(p) = s.try_acquire_permit_for(Duration::from_micros(50)) {
                                break Some(p);
                            }
                        },
                        _ => {
                            s.acquire();
                            None
                        }
                    };
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    assert!(now <= PERMITS, "{now} holders");
                    if i % 200 == 0 {
                        thread::yield_now();
                    }
                    active.fetch_sub(1, Ordering::SeqCst);
                    match permit {
                        Some(p) => drop(p),
                        None => s.release(1),
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(s.available_permits(), PERMITS as u32);
    assert!(peak.load(Ordering::SeqCst) >= 1);
}
