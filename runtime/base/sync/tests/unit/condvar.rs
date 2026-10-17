use std::collections::VecDeque;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use marlin_sync::{Condvar, Mutex};

static STATIC_CONDVAR: Condvar = Condvar::new();

#[test]
fn producer_consumer() {
    const ITEMS: usize = 10_000;
    const CONSUMERS: usize = 4;

    let queue = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
    let consumers: Vec<_> = (0..CONSUMERS)
        .map(|_| {
            let queue = queue.clone();
            thread::spawn(move || {
                let (lock, cv) = &*queue;
                let mut sum = 0usize;
                loop {
                    let mut q = lock.lock();
                    cv.wait_while(&mut q, |q| q.is_empty());
                    match q.pop_front().unwrap() {
                        None => return sum,
                        Some(v) => sum += v,
                    }
                }
            })
        })
        .collect();

    let (lock, cv) = &*queue;
    for i in 0..ITEMS {
        lock.lock().push_back(Some(i));
        cv.notify_one();
    }
    for _ in 0..CONSUMERS {
        lock.lock().push_back(None);
    }
    cv.notify_all();

    let total: usize = consumers.into_iter().map(|c| c.join().unwrap()).sum();
    assert_eq!(total, ITEMS * (ITEMS - 1) / 2);
}

#[test]
fn notify_all_wakes_everyone() {
    let state = Arc::new((Mutex::new(false), Condvar::new()));
    let waiters: Vec<_> = (0..6)
        .map(|_| {
            let state = state.clone();
            thread::spawn(move || {
                let (lock, cv) = &*state;
                let mut go = lock.lock();
                while !*go {
                    cv.wait(&mut go);
                }
            })
        })
        .collect();
    thread::sleep(Duration::from_millis(20));
    *state.0.lock() = true;
    state.1.notify_all();
    for w in waiters {
        w.join().unwrap();
    }
}

#[test]
fn wait_timeout_times_out() {
    let m = Mutex::new(());
    let mut g = m.lock();
    let timeout = Duration::from_millis(30);
    let start = Instant::now();
    let r = STATIC_CONDVAR.wait_timeout_while(&mut g, timeout, |_| true);
    assert!(r.timed_out());
    assert!(start.elapsed() >= timeout, "{:?}", start.elapsed());
    // The mutex is held again after the wait.
    assert!(m.is_locked());
}

#[test]
fn wait_timeout_while_returns_when_condition_met() {
    let state = Arc::new((Mutex::new(0), Condvar::new()));
    let setter = {
        let state = state.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            *state.0.lock() = 1;
            state.1.notify_one();
        })
    };
    let (lock, cv) = &*state;
    let mut g = lock.lock();
    let r = cv.wait_timeout_while(&mut g, Duration::from_secs(10), |v| *v == 0);
    assert!(!r.timed_out());
    assert_eq!(*g, 1);
    drop(g);
    setter.join().unwrap();
}
