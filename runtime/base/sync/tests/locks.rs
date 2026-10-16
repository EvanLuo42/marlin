use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use marlin_sync::{Mutex, RawMutex, RwLock};

const THREADS: usize = 8;
const ITERS: usize = 20_000;

// Constructors must be usable in statics.
static STATIC_MUTEX: Mutex<u64> = Mutex::new(0);
static STATIC_RWLOCK: RwLock<u64> = RwLock::new(0);
static STATIC_RAW: RawMutex = RawMutex::new();

#[test]
fn mutex_basic() {
    let m = Mutex::new(1);
    {
        let mut g = m.lock();
        *g += 1;
        assert!(m.is_locked());
        assert!(m.try_lock().is_none());
    }
    assert!(!m.is_locked());
    assert_eq!(*m.try_lock().unwrap(), 2);
    assert_eq!(m.into_inner(), 2);
}

#[test]
fn mutex_contention() {
    // A non-atomic read-modify-write with a yield in the middle: lost updates if exclusion fails.
    let m = Arc::new(Mutex::new(0usize));
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let m = m.clone();
            thread::spawn(move || {
                for i in 0..ITERS {
                    let mut g = m.lock();
                    let v = *g;
                    if i % 1000 == 0 {
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
    assert_eq!(*m.lock(), THREADS * ITERS);
}

#[test]
fn static_mutex() {
    let handles: Vec<_> = (0..4)
        .map(|_| {
            thread::spawn(|| {
                for _ in 0..1000 {
                    *STATIC_MUTEX.lock() += 1;
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(*STATIC_MUTEX.lock(), 4000);
}

#[test]
fn raw_mutex_sleeper_is_woken() {
    STATIC_RAW.lock();
    let waiter = thread::spawn(|| {
        STATIC_RAW.lock();
        // SAFETY: locked just above by this thread.
        unsafe { STATIC_RAW.unlock() };
    });
    // Long enough for the waiter to exhaust its spin and sleep.
    thread::sleep(Duration::from_millis(50));
    // SAFETY: locked at the top of this test by this thread.
    unsafe { STATIC_RAW.unlock() };
    waiter.join().unwrap();
    assert!(!STATIC_RAW.is_locked());
}

#[test]
fn guard_can_unlock_on_another_thread() {
    let m = Arc::new(Mutex::new(5));
    let guard = m.lock();
    thread::scope(|s| {
        s.spawn(move || drop(guard));
    });
    assert!(!m.is_locked());
}

#[test]
fn rwlock_readers_share_writers_exclude() {
    let l = RwLock::new(0);
    {
        let r1 = l.read();
        let r2 = l.try_read().unwrap();
        assert_eq!(*r1 + *r2, 0);
        assert!(l.try_write().is_none());
    }
    {
        let mut w = l.write();
        *w = 7;
        assert!(l.try_read().is_none());
        assert!(l.try_write().is_none());
    }
    assert_eq!(*l.read(), 7);
    *STATIC_RWLOCK.write() += 1;
    assert_eq!(*STATIC_RWLOCK.read(), 1);
}

#[test]
fn rwlock_contention() {
    // Writers keep two fields equal; a reader that sees them differ saw a torn write.
    let l = Arc::new(RwLock::new((0usize, 0usize)));
    let reads = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let l = l.clone();
            let reads = reads.clone();
            thread::spawn(move || {
                for i in 0..ITERS / 4 {
                    if t % 2 == 0 {
                        let mut w = l.write();
                        w.0 += 1;
                        if i % 500 == 0 {
                            thread::yield_now();
                        }
                        w.1 += 1;
                    } else {
                        let r = l.read();
                        assert_eq!(r.0, r.1);
                        reads.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let writers = THREADS.div_ceil(2);
    assert_eq!(*l.read(), (writers * ITERS / 4, writers * ITERS / 4));
    assert_eq!(
        reads.load(Ordering::Relaxed),
        (THREADS - writers) * ITERS / 4
    );
}

#[test]
fn rwlock_waiting_writer_blocks_new_readers() {
    let l = Arc::new(RwLock::new(0));
    let reader = l.read();
    let wrote = Arc::new(AtomicBool::new(false));
    let writer = {
        let l = l.clone();
        let wrote = wrote.clone();
        thread::spawn(move || {
            *l.write() = 1;
            wrote.store(true, Ordering::Release);
        })
    };
    // Wait until the writer has announced itself: then new readers must back off.
    let mut tries = 0;
    while l.try_read().is_some() {
        tries += 1;
        assert!(tries < 1000, "writer never started waiting");
        thread::sleep(Duration::from_millis(1));
    }
    assert!(!wrote.load(Ordering::Acquire));
    drop(reader);
    writer.join().unwrap();
    assert_eq!(*l.read(), 1);
}

#[test]
fn rwlock_writers_and_blocked_readers_all_finish() {
    // Readers and writers pile up behind a held write lock, then all must get through.
    let l = Arc::new(RwLock::new(0usize));
    let w = l.write();
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let l = l.clone();
            thread::spawn(move || {
                if t % 2 == 0 {
                    *l.write() += 1;
                } else {
                    let _ = *l.read();
                }
            })
        })
        .collect();
    thread::sleep(Duration::from_millis(50));
    drop(w);
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(*l.read(), THREADS.div_ceil(2));
}
