use std::cell::Cell;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use marlin_sync::ReentrantMutex;

static STATIC_REENTRANT: ReentrantMutex<Cell<u64>> = ReentrantMutex::new(Cell::new(0));

/// Timed waits may end a little early by the OS's clock; allow 1 ms, as the PAL tests do.
fn assert_waited(start: Instant, timeout: Duration) {
    let elapsed = start.elapsed();
    assert!(elapsed + Duration::from_millis(1) >= timeout, "{elapsed:?}");
}

#[test]
fn relocks_on_the_owning_thread() {
    let m = ReentrantMutex::new(Cell::new(0));
    let a = m.lock();
    let b = m.lock();
    let c = m.try_lock().unwrap();
    let d = m.try_lock_for(Duration::ZERO).unwrap();
    a.set(a.get() + 1);
    b.set(b.get() + 1);
    assert_eq!(c.get() + d.get(), 4);
    drop(b);
    drop(a);
    drop(d);
    assert!(m.is_locked());
    drop(c);
    assert!(!m.is_locked());
    assert_eq!(m.into_inner().get(), 2);
}

#[test]
fn other_threads_are_excluded() {
    let m = ReentrantMutex::new(Cell::new(0));
    let outer = m.lock();
    let inner = m.lock();
    thread::scope(|s| {
        s.spawn(|| {
            assert!(m.try_lock().is_none());
            let timeout = Duration::from_millis(20);
            let start = Instant::now();
            assert!(m.try_lock_for(timeout).is_none());
            assert_waited(start, timeout);
        });
    });
    drop(inner);
    // Still held once.
    thread::scope(|s| {
        s.spawn(|| assert!(m.try_lock().is_none()));
    });
    drop(outer);
    thread::scope(|s| {
        s.spawn(|| assert!(m.try_lock().is_some()));
    });
}

#[test]
fn blocked_thread_gets_it_after_full_release() {
    let m = Arc::new(ReentrantMutex::new(Cell::new(0)));
    let outer = m.lock();
    let inner = m.lock();
    let waiter = {
        let m = m.clone();
        thread::spawn(move || {
            let g = m.lock();
            g.get()
        })
    };
    thread::sleep(Duration::from_millis(20));
    inner.set(7);
    drop(inner);
    thread::sleep(Duration::from_millis(10));
    assert!(!waiter.is_finished());
    drop(outer);
    assert_eq!(waiter.join().unwrap(), 7);
}

#[test]
fn contention_with_nesting() {
    // Non-atomic read-modify-write split across a nested lock: lost updates if exclusion fails.
    const THREADS: usize = 8;
    const ITERS: usize = 5_000;
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            thread::spawn(|| {
                for i in 0..ITERS {
                    let outer = STATIC_REENTRANT.lock();
                    let v = outer.get();
                    if i % 1000 == 0 {
                        thread::yield_now();
                    }
                    let inner = STATIC_REENTRANT.lock();
                    inner.set(v + 1);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(STATIC_REENTRANT.lock().get(), (THREADS * ITERS) as u64);
}
