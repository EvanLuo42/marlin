//! Runs on `MarlinHeap`, so jobs that allocate, park, resume on another worker and allocate again
//! also exercise the allocator's thread-local heaps under fiber migration.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use marlin_jobs::{Config, JobSystem, Priority};
use marlin_std::mem::MarlinHeap;

#[global_allocator]
static HEAP: MarlinHeap = MarlinHeap::new();

fn system(workers: usize, fibers: usize) -> JobSystem {
    JobSystem::new(Config {
        workers,
        fibers,
        fiber_stack_size: 128 << 10,
        pin_workers: false,
    })
    .unwrap()
}

#[test]
fn scope_runs_everything_and_borrows() {
    let jobs = system(4, 64);
    let mut out = vec![0u64; 1000];
    jobs.scope(|s| {
        for (i, slot) in out.iter_mut().enumerate() {
            s.spawn(move || *slot = (i * i) as u64);
        }
    });
    assert!(out.iter().enumerate().all(|(i, &v)| v == (i * i) as u64));
}

/// Sums 0..n by splitting in half and waiting on the halves from inside the job.
// The `vec!`s are deliberate heap allocations on both sides of the wait.
#[allow(clippy::useless_vec)]
fn tree_sum(jobs: &JobSystem, lo: u64, hi: u64) -> u64 {
    if hi - lo <= 64 {
        // Allocate on both sides of the waits above us.
        let v: Vec<u64> = (lo..hi).collect();
        return v.iter().sum();
    }
    let mid = lo + (hi - lo) / 2;
    let before = vec![lo; 16];
    let (mut a, mut b) = (0, 0);
    jobs.scope(|s| {
        s.spawn(|| a = tree_sum(jobs, lo, mid));
        s.spawn(|| b = tree_sum(jobs, mid, hi));
    });
    let after = vec![hi; 16];
    assert!(before.iter().all(|&x| x == lo) && after.iter().all(|&x| x == hi));
    a + b
}

#[test]
fn nested_waits_park_fibers() {
    let jobs = system(4, 1024);
    let n = 64 * 256;
    let mut result = 0;
    jobs.scope(|s| s.spawn(|| result = tree_sum(&jobs, 0, n)));
    assert_eq!(result, n * (n - 1) / 2);
}

#[test]
fn single_worker_still_completes_nested_waits() {
    // With one worker, a waiting job must park, or its children could never run.
    let jobs = system(1, 256);
    let mut result = 0;
    jobs.scope(|s| s.spawn(|| result = tree_sum(&jobs, 0, 64 * 32)));
    assert_eq!(result, (64 * 32) * (64 * 32 - 1) / 2);
}

#[test]
fn waits_resume_on_other_workers() {
    let jobs = system(4, 512);
    let threads = Mutex::new(HashSet::new());
    let migrated = AtomicUsize::new(0);
    let inner_runs = AtomicUsize::new(0);
    jobs.scope(|s| {
        for _ in 0..64 {
            s.spawn(|| {
                let before = marlin_std::thread::current::id();
                threads.lock().unwrap().insert(before);
                let held = vec![before; 64];
                jobs.scope(|inner| {
                    for _ in 0..8 {
                        inner.spawn(|| {
                            let v: Vec<u8> = vec![7; 1000];
                            std::hint::black_box(v);
                            inner_runs.fetch_add(1, Ordering::Relaxed);
                        });
                    }
                });
                // Memory allocated before the wait is intact and freeable on the new thread.
                assert!(held.iter().all(|&id| id == before));
                drop(held);
                if marlin_std::thread::current::id() != before {
                    migrated.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    assert_eq!(inner_runs.load(Ordering::Relaxed), 64 * 8);
    // How work spreads depends on load, so this is reported rather than asserted.
    println!(
        "{} worker threads ran jobs; {} of 64 resumed on another worker",
        threads.lock().unwrap().len(),
        migrated.load(Ordering::Relaxed)
    );
}

#[test]
fn parallel_for_visits_each_index_once() {
    let jobs = system(4, 64);
    let hits: Vec<AtomicU64> = (0..10_000).map(|_| AtomicU64::new(0)).collect();
    jobs.parallel_for(0..10_000, 37, |i| {
        hits[i].fetch_add(1, Ordering::Relaxed);
    });
    assert!(hits.iter().all(|h| h.load(Ordering::Relaxed) == 1));
}

#[test]
fn local_queues_grow_while_stolen() {
    // One job pushes far more children than a local queue starts with, so the queue grows
    // while the other workers steal from it; every child must run exactly once.
    let jobs = system(4, 64);
    for round in 0..4 {
        let hits: Vec<AtomicU64> = (0..50_000).map(|_| AtomicU64::new(0)).collect();
        jobs.scope(|s| {
            s.spawn(|| {
                jobs.scope(|inner| {
                    for hit in &hits {
                        inner.spawn(move || {
                            hit.fetch_add(1, Ordering::Relaxed);
                        });
                    }
                });
            });
        });
        assert!(
            hits.iter().all(|h| h.load(Ordering::Relaxed) == 1),
            "round {round}"
        );
    }
}

#[test]
fn many_outside_threads_share_a_system() {
    let jobs = system(4, 256);
    let total = AtomicU64::new(0);
    std::thread::scope(|threads| {
        for t in 0..8u64 {
            let (jobs, total) = (&jobs, &total);
            threads.spawn(move || {
                for round in 0..50u64 {
                    jobs.scope(|s| {
                        for i in 0..20u64 {
                            s.spawn_with(
                                [Priority::High, Priority::Normal, Priority::Low][(i % 3) as usize],
                                move || {
                                    total.fetch_add(t * 1000 + round + i, Ordering::Relaxed);
                                },
                            );
                        }
                    });
                }
            });
        }
    });
    let expected: u64 = (0..8u64)
        .flat_map(|t| (0..50u64).flat_map(move |r| (0..20u64).map(move |i| t * 1000 + r + i)))
        .sum();
    assert_eq!(total.load(Ordering::Relaxed), expected);
}

#[test]
fn systems_start_and_stop_repeatedly() {
    for _ in 0..20 {
        let jobs = system(3, 16);
        let n = AtomicUsize::new(0);
        jobs.parallel_for(0..100, 10, |_| {
            n.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(n.load(Ordering::Relaxed), 100);
    }
}

/// `cargo test --release -p marlin-jobs --test jobs -- --ignored --nocapture`
#[test]
#[ignore]
fn bench() {
    use std::time::Instant;

    let jobs = system(0, 512);
    let n = 200_000;
    let counter = AtomicUsize::new(0);
    let start = Instant::now();
    jobs.scope(|s| {
        for _ in 0..n {
            s.spawn(|| {
                counter.fetch_add(1, Ordering::Relaxed);
            });
        }
    });
    let per_job = start.elapsed().as_nanos() as f64 / n as f64;
    println!(
        "{} workers: {per_job:.0} ns per empty job (spawn + run)",
        jobs.worker_count()
    );

    let start = Instant::now();
    let mut result = 0;
    jobs.scope(|s| s.spawn(|| result = tree_sum(&jobs, 0, 64 * 4096)));
    println!("fork-join tree, 4095 nested waits: {:?}", start.elapsed());
    assert_eq!(result, (64 * 4096) * (64 * 4096 - 1) / 2);
}
