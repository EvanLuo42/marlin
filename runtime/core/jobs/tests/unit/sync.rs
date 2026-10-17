//! Fiber-aware locks and signals: contended in jobs they park the fiber, not the worker.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use marlin_jobs::sync::{Condvar, Event, Mutex, RwLock};

use crate::support::system;

#[test]
fn mutex_counts_under_contention() {
    let jobs = system(4, 64, 1024);
    let count = Mutex::new(0u64);
    jobs.parallel_for(0..20_000, 7, |_| *count.lock() += 1);
    assert_eq!(*count.lock(), 20_000);
    assert!(!count.is_locked());
}

#[test]
fn contended_lock_parks_instead_of_blocking() {
    // One worker: while the holder waits on its own children with the lock held, other jobs
    // contend for it. If they blocked the thread, the holder's children could never run.
    let jobs = system(1, 64, 1024);
    let lock = Mutex::new(Vec::new());
    jobs.scope(|s| {
        s.spawn(|| {
            let mut guard = lock.lock();
            jobs.scope(|inner| {
                for _ in 0..16 {
                    inner.spawn(|| assert!(lock.try_lock().is_none()));
                }
            });
            guard.push(0);
        });
        for i in 1..=16 {
            let lock = &lock;
            s.spawn(move || lock.lock().push(i));
        }
    });
    let mut v = lock.into_inner();
    v.sort();
    assert_eq!(v, (0..=16).collect::<Vec<_>>());
}

#[test]
fn guards_released_on_another_worker() {
    let jobs = system(4, 64, 1024);
    let lock = Mutex::new(0u32);
    let moved = AtomicUsize::new(0);
    jobs.scope(|s| {
        for _ in 0..64 {
            s.spawn(|| {
                let before = marlin_std::thread::current::id();
                let mut guard = lock.lock();
                // Wait while holding the guard: the fiber may resume elsewhere.
                jobs.scope(|inner| {
                    for _ in 0..4 {
                        inner.spawn(|| std::hint::black_box(()));
                    }
                });
                *guard += 1;
                if marlin_std::thread::current::id() != before {
                    moved.fetch_add(1, Ordering::Relaxed);
                }
                drop(guard);
            });
        }
    });
    assert_eq!(*lock.lock(), 64);
    println!(
        "{} of 64 guards dropped on another thread",
        moved.load(Ordering::Relaxed)
    );
}

#[test]
fn outside_threads_block_and_share_with_jobs() {
    let jobs = system(2, 64, 1024);
    let count = Mutex::new(0u64);
    std::thread::scope(|threads| {
        for _ in 0..4 {
            threads.spawn(|| {
                for _ in 0..2000 {
                    *count.lock() += 1;
                }
            });
        }
        jobs.parallel_for(0..8000, 13, |_| *count.lock() += 1);
    });
    assert_eq!(*count.lock(), 16_000);
}

#[test]
fn rwlock_readers_share_writers_exclude() {
    let jobs = system(4, 64, 1024);
    let lock = RwLock::new(0u64);
    let readers = AtomicU32::new(0);
    let max_readers = AtomicU32::new(0);
    let writing = AtomicBool::new(false);
    jobs.parallel_for(0..4000, 1, |i| {
        if i % 8 == 0 {
            let mut w = lock.write();
            assert!(!writing.swap(true, Ordering::SeqCst));
            assert_eq!(readers.load(Ordering::SeqCst), 0);
            *w += 1;
            writing.store(false, Ordering::SeqCst);
        } else {
            let r = lock.read();
            assert!(!writing.load(Ordering::SeqCst));
            let n = readers.fetch_add(1, Ordering::SeqCst) + 1;
            max_readers.fetch_max(n, Ordering::Relaxed);
            std::hint::black_box(*r);
            readers.fetch_sub(1, Ordering::SeqCst);
        }
    });
    assert_eq!(*lock.read(), 500);
    assert!(lock.try_write().is_some());
    println!(
        "up to {} readers at once",
        max_readers.load(Ordering::Relaxed)
    );
}

#[test]
fn rwlock_writer_waits_for_readers_across_waits() {
    // Readers hold the lock across job waits; a writer queues and is let in once all leave.
    let jobs = system(2, 64, 1024);
    let lock = RwLock::new(Vec::new());
    jobs.scope(|s| {
        for i in 0..8 {
            let lock = &lock;
            let jobs = &jobs;
            s.spawn(move || {
                if i == 4 {
                    lock.write().push(i);
                    return;
                }
                let r = lock.read();
                jobs.scope(|inner| inner.spawn(|| std::thread::sleep(Duration::from_millis(1))));
                std::hint::black_box(r.len());
            });
        }
    });
    assert_eq!(lock.into_inner(), vec![4]);
}

#[test]
fn condvar_hands_items_between_jobs() {
    let jobs = system(3, 64, 1024);
    let queue = Mutex::new(Vec::<u32>::new());
    let ready = Condvar::new();
    let received = Mutex::new(Vec::new());
    jobs.scope(|s| {
        // Consumers outnumber workers: they must park in `wait`.
        for _ in 0..8 {
            s.spawn(|| {
                loop {
                    let mut q = queue.lock();
                    ready.wait_while(&mut q, |q| q.is_empty());
                    let item = q.pop().unwrap();
                    drop(q);
                    if item == u32::MAX {
                        return;
                    }
                    received.lock().push(item);
                }
            });
        }
        s.spawn(|| {
            for i in 0..1000 {
                queue.lock().push(i);
                ready.notify_one();
            }
            for _ in 0..8 {
                queue.lock().insert(0, u32::MAX);
                ready.notify_all();
            }
        });
    });
    let mut got = received.into_inner();
    got.sort();
    assert_eq!(got, (0..1000).collect::<Vec<_>>());
}

#[test]
fn condvar_between_a_thread_and_jobs() {
    let jobs = system(2, 64, 1024);
    let state = Mutex::new(0u32);
    let changed = Condvar::new();
    std::thread::scope(|threads| {
        threads.spawn(|| {
            // An outside thread waits on a value only jobs set.
            let mut s = state.lock();
            changed.wait_while(&mut s, |s| *s < 100);
        });
        jobs.parallel_for(0..100, 1, |_| {
            *state.lock() += 1;
            changed.notify_all();
        });
    });
    assert_eq!(*state.lock(), 100);
}

#[test]
fn event_releases_every_waiter() {
    let jobs = system(2, 64, 1024);
    let gate = Event::new(false);
    let passed = AtomicU32::new(0);
    let threads = Mutex::new(HashSet::new());
    jobs.scope(|s| {
        for _ in 0..32 {
            s.spawn(|| {
                gate.wait();
                threads.lock().insert(marlin_std::thread::current::id());
                passed.fetch_add(1, Ordering::Relaxed);
            });
        }
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(5));
            assert_eq!(passed.load(Ordering::Relaxed), 0);
            gate.set();
        });
    });
    assert_eq!(passed.load(Ordering::Relaxed), 32);
    assert!(gate.is_set());
    gate.reset();
    assert!(!gate.is_set());
    // Set again: an outside thread passes at once.
    gate.set();
    gate.wait();
}
