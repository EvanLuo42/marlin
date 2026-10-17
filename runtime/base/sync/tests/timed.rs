use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use marlin_sync::{Mutex, RawMutex, RawRwLock, RwLock};

/// Timed waits may end a little early by the OS's clock; allow 1 ms, as the PAL tests do.
fn assert_waited(start: Instant, timeout: Duration) {
    let elapsed = start.elapsed();
    assert!(elapsed + Duration::from_millis(1) >= timeout, "{elapsed:?}");
}

const TIMEOUT: Duration = Duration::from_millis(30);

#[test]
fn mutex_try_lock_for_times_out() {
    let m = Mutex::new(0);
    let _held = m.lock();
    thread::scope(|s| {
        s.spawn(|| {
            let start = Instant::now();
            assert!(m.try_lock_for(TIMEOUT).is_none());
            assert_waited(start, TIMEOUT);
            assert!(m.try_lock_for(Duration::ZERO).is_none());
        });
    });
    // The waiter left the word marked contended; the lock must still work normally.
    drop(_held);
    assert!(m.try_lock_for(Duration::ZERO).is_some());
    assert!(!m.is_locked());
}

#[test]
fn mutex_try_lock_for_succeeds_when_released() {
    let m = Arc::new(Mutex::new(0));
    let held = m.lock();
    let waiter = {
        let m = m.clone();
        thread::spawn(move || {
            let mut g = m.try_lock_for(Duration::from_secs(10)).unwrap();
            *g += 1;
        })
    };
    thread::sleep(Duration::from_millis(20));
    drop(held);
    waiter.join().unwrap();
    assert_eq!(*m.lock(), 1);
}

#[test]
fn raw_mutex_try_lock_for() {
    let raw = RawMutex::new();
    assert!(raw.try_lock_for(TIMEOUT));
    let start = Instant::now();
    assert!(!raw.try_lock_for(TIMEOUT));
    assert_waited(start, TIMEOUT);
    // SAFETY: locked above by this thread.
    unsafe { raw.unlock() };
    assert!(!raw.is_locked());
}

#[test]
fn rwlock_try_write_for_times_out_behind_reader() {
    let l = RwLock::new(0);
    let reader = l.read();
    thread::scope(|s| {
        s.spawn(|| {
            let start = Instant::now();
            assert!(l.try_write_for(TIMEOUT).is_none());
            assert_waited(start, TIMEOUT);
        });
    });
    drop(reader);
    // The timed-out writer may leave its waiting bit behind; the lock must recover.
    *l.try_write_for(Duration::ZERO).unwrap() = 1;
    assert_eq!(*l.read(), 1);
    assert_eq!(*l.try_read_for(Duration::ZERO).unwrap(), 1);
}

#[test]
fn rwlock_try_read_for_times_out_behind_writer() {
    let l = RwLock::new(0);
    let writer = l.write();
    thread::scope(|s| {
        s.spawn(|| {
            let start = Instant::now();
            assert!(l.try_read_for(TIMEOUT).is_none());
            assert_waited(start, TIMEOUT);
            assert!(l.try_write_for(Duration::from_millis(5)).is_none());
        });
    });
    drop(writer);
    assert!(l.try_read_for(Duration::ZERO).is_some());
    assert!(l.try_write_for(Duration::ZERO).is_some());
}

#[test]
fn rwlock_timed_waiters_succeed_when_released() {
    let l = Arc::new(RwLock::new(0));
    let writer = l.write();
    let waiters: Vec<_> = (0..4)
        .map(|t| {
            let l = l.clone();
            thread::spawn(move || {
                if t % 2 == 0 {
                    *l.try_write_for(Duration::from_secs(10)).unwrap() += 1;
                } else {
                    let _ = *l.try_read_for(Duration::from_secs(10)).unwrap();
                }
            })
        })
        .collect();
    thread::sleep(Duration::from_millis(20));
    drop(writer);
    for w in waiters {
        w.join().unwrap();
    }
    assert_eq!(*l.read(), 2);
}

#[test]
fn raw_rwlock_timed() {
    let raw = RawRwLock::new();
    assert!(raw.try_read_for(Duration::ZERO));
    assert!(raw.try_read_for(Duration::ZERO));
    assert!(!raw.try_write_for(Duration::from_millis(5)));
    // SAFETY: two read locks taken above by this thread.
    unsafe {
        raw.read_unlock();
        raw.read_unlock();
    }
    assert!(raw.try_write_for(Duration::ZERO));
    assert!(!raw.try_read_for(Duration::from_millis(5)));
    // SAFETY: write-locked above by this thread.
    unsafe { raw.write_unlock() };
    assert!(!raw.is_locked());
}

#[test]
fn mutex_timed_and_blocking_lockers_mixed() {
    // Lockers that give up constantly must not swallow wakeups the blocking lockers need: every
    // blocking `lock` has to get through.
    const THREADS: usize = 8;
    const ITERS: usize = 3_000;
    let m = Arc::new(Mutex::new(0usize));
    let timed_wins = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let (m, timed_wins) = (m.clone(), timed_wins.clone());
            thread::spawn(move || {
                for i in 0..ITERS {
                    let mut g = if t % 2 == 0 {
                        m.lock()
                    } else {
                        match m.try_lock_for(Duration::from_micros(10)) {
                            Some(g) => {
                                timed_wins.fetch_add(1, Ordering::Relaxed);
                                g
                            }
                            None => continue,
                        }
                    };
                    let v = *g;
                    if i % 500 == 0 {
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
    let blocking = THREADS.div_ceil(2) * ITERS;
    assert_eq!(*m.lock(), blocking + timed_wins.load(Ordering::Relaxed));
}

#[test]
fn rwlock_timed_and_blocking_mixed() {
    // As above, for writers and readers; writers keep both fields equal.
    const THREADS: usize = 8;
    const ITERS: usize = 2_000;
    let l = Arc::new(RwLock::new((0usize, 0usize)));
    let timed_writes = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let (l, timed_writes) = (l.clone(), timed_writes.clone());
            thread::spawn(move || {
                let short = Duration::from_micros(10);
                for i in 0..ITERS {
                    match t % 4 {
                        0 => {
                            let mut w = l.write();
                            w.0 += 1;
                            w.1 += 1;
                        }
                        1 => {
                            if let Some(mut w) = l.try_write_for(short) {
                                w.0 += 1;
                                if i % 500 == 0 {
                                    thread::yield_now();
                                }
                                w.1 += 1;
                                timed_writes.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        2 => {
                            let r = l.read();
                            assert_eq!(r.0, r.1);
                        }
                        _ => {
                            if let Some(r) = l.try_read_for(short) {
                                assert_eq!(r.0, r.1);
                            }
                        }
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let expected = (THREADS / 4) * ITERS + timed_writes.load(Ordering::Relaxed);
    assert_eq!(*l.read(), (expected, expected));
}
