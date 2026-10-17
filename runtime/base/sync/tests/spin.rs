use std::sync::Arc;
use std::thread;

use marlin_sync::SpinLock;

static STATIC_SPIN: SpinLock<u64> = SpinLock::new(0);

#[test]
fn basic() {
    let l = SpinLock::new(1);
    {
        let mut g = l.lock();
        *g += 1;
        assert!(l.is_locked());
        assert!(l.try_lock().is_none());
    }
    assert!(!l.is_locked());
    assert_eq!(*l.try_lock().unwrap(), 2);
    assert_eq!(format!("{l:?}"), "SpinLock { data: 2, .. }");
    assert_eq!(l.into_inner(), 2);
}

#[test]
fn contention() {
    // A non-atomic read-modify-write with a yield in the middle: lost updates if exclusion fails.
    const THREADS: usize = 8;
    const ITERS: usize = 20_000;
    let l = Arc::new(SpinLock::new(0usize));
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let l = l.clone();
            thread::spawn(move || {
                for i in 0..ITERS {
                    let mut g = l.lock();
                    let v = *g;
                    if i % 5000 == 0 {
                        thread::yield_now();
                    }
                    *g = v + 1;
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(*l.lock(), THREADS * ITERS);
}

#[test]
fn static_spin_lock() {
    let handles: Vec<_> = (0..4)
        .map(|_| {
            thread::spawn(|| {
                for _ in 0..1000 {
                    *STATIC_SPIN.lock() += 1;
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(*STATIC_SPIN.lock(), 4000);
}

#[test]
fn guard_can_unlock_on_another_thread() {
    let l = SpinLock::new(5);
    let guard = l.lock();
    thread::scope(|s| {
        s.spawn(move || drop(guard));
    });
    assert!(!l.is_locked());
}
