use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use marlin_thread::{LocalKey, spawn};

static COUNTER: LocalKey<Cell<u32>> = LocalKey::new(|| Cell::new(0));

#[test]
fn values_are_per_thread() {
    COUNTER.with(|c| c.set(c.get() + 5));
    let others: Vec<_> = (0..8)
        .map(|i| {
            spawn(move || {
                COUNTER.with(|c| c.set(c.get() + i));
                COUNTER.with(|c| c.get())
            })
        })
        .collect();
    let seen: Vec<u32> = others.into_iter().map(|h| h.join()).collect();
    assert_eq!(seen, (0..8).collect::<Vec<_>>());
    assert_eq!(COUNTER.with(|c| c.get()), 5);
}

static CREATED: AtomicUsize = AtomicUsize::new(0);
static DROPPED: AtomicUsize = AtomicUsize::new(0);

struct Tracked;

impl Drop for Tracked {
    fn drop(&mut self) {
        DROPPED.fetch_add(1, Ordering::SeqCst);
    }
}

static TRACKED: LocalKey<Tracked> = LocalKey::new(|| {
    CREATED.fetch_add(1, Ordering::SeqCst);
    Tracked
});

#[test]
fn values_are_created_lazily_and_dropped_at_thread_exit() {
    let handles: Vec<_> = (0..4)
        .map(|_| {
            spawn(|| {
                TRACKED.with(|_| {});
                TRACKED.with(|_| {});
            })
        })
        .collect();
    // A thread that never touches the key creates nothing.
    spawn(|| {}).join();
    for handle in handles {
        handle.join();
    }
    assert_eq!(CREATED.load(Ordering::SeqCst), 4);
    assert_eq!(DROPPED.load(Ordering::SeqCst), 4);
}
