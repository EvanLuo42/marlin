//! The fiber pool: growing on demand, and waits at the cap that help instead of parking.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use marlin_jobs::sync::Mutex;
use marlin_jobs::{Config, JobSystem};
use marlin_std::mem::MarlinHeap;

#[global_allocator]
static HEAP: MarlinHeap = MarlinHeap::new();

fn system(workers: usize, fibers: usize, max_fibers: usize) -> JobSystem {
    system_with_stack(workers, fibers, max_fibers, 128 << 10)
}

fn system_with_stack(workers: usize, fibers: usize, max_fibers: usize, stack: usize) -> JobSystem {
    JobSystem::new(Config {
        workers,
        fibers,
        max_fibers,
        fiber_stack_size: stack,
        pin_workers: false,
    })
    .unwrap()
}

/// A chain of `depth` jobs, each waiting for the next: every one of them parks at once.
fn chain(jobs: &JobSystem, depth: usize, done: &AtomicUsize) {
    if depth == 0 {
        return;
    }
    jobs.scope(|s| s.spawn(|| chain(jobs, depth - 1, done)));
    done.fetch_add(1, Ordering::Relaxed);
}

fn tree_sum(jobs: &JobSystem, lo: u64, hi: u64) -> u64 {
    if hi - lo <= 16 {
        return (lo..hi).sum();
    }
    let mid = lo + (hi - lo) / 2;
    let (mut a, mut b) = (0, 0);
    jobs.scope(|s| {
        s.spawn(|| a = tree_sum(jobs, lo, mid));
        s.spawn(|| b = tree_sum(jobs, mid, hi));
    });
    a + b
}

#[test]
fn pool_grows_for_deep_waits() {
    // 200 nested waits hold 200 fibers at once: far more than the 8 to start with.
    let jobs = system(2, 8, 1024);
    assert_eq!(jobs.fiber_count(), 8);
    let done = AtomicUsize::new(0);
    jobs.scope(|s| s.spawn(|| chain(&jobs, 200, &done)));
    assert_eq!(done.load(Ordering::Relaxed), 200);
    let grown = jobs.fiber_count();
    assert!((200..=1024).contains(&grown), "{grown} fibers");
    // Fibers are reused, not made again.
    done.store(0, Ordering::Relaxed);
    jobs.scope(|s| s.spawn(|| chain(&jobs, 200, &done)));
    assert_eq!(done.load(Ordering::Relaxed), 200);
    assert_eq!(jobs.fiber_count(), grown);
}

#[test]
fn deep_waits_at_the_cap_help_instead() {
    // 300 nested waits with at most 6 fibers: waits at the cap run the next job nested, on
    // their own stack, so the stacks must be deep enough.
    for workers in [1, 3] {
        let jobs = system_with_stack(workers, 4, 6, 4 << 20);
        let done = AtomicUsize::new(0);
        jobs.scope(|s| s.spawn(|| chain(&jobs, 300, &done)));
        assert_eq!(done.load(Ordering::Relaxed), 300);
        assert!(jobs.fiber_count() <= 6);
    }
}

#[test]
fn wide_fork_join_at_the_cap() {
    // A tree of 4095 waits on a pool far too small to park them all.
    for workers in [1, 2, 4] {
        let jobs = system(workers, 4, 8);
        for _ in 0..4 {
            let n = 16 * 4096;
            let mut sum = 0;
            jobs.scope(|s| s.spawn(|| sum = tree_sum(&jobs, 0, n)));
            assert_eq!(sum, n * (n - 1) / 2);
        }
        assert!(jobs.fiber_count() <= 8);
    }
}

#[test]
fn locks_contended_at_the_cap() {
    // Jobs hold a fiber-aware lock across a wait, with too few fibers to park every waiter:
    // waiters at the cap must still resume the holder when its wait is over.
    let jobs = system(2, 4, 5);
    let lock = Mutex::new(0u64);
    let inner = AtomicU64::new(0);
    jobs.scope(|s| {
        for _ in 0..64 {
            s.spawn(|| {
                let mut guard = lock.lock();
                jobs.scope(|s| {
                    s.spawn(|| {
                        inner.fetch_add(1, Ordering::Relaxed);
                    });
                });
                *guard += 1;
            });
        }
    });
    assert_eq!(*lock.lock(), 64);
    assert_eq!(inner.load(Ordering::Relaxed), 64);
}
