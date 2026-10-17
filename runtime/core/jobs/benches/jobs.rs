//! Job overhead: 200,000 empty jobs in one scope (spawn + run), a fork-join tree of 4095 nested
//! waits, and the empty jobs again with this thread registered as the main thread. On
//! `MarlinHeap`, like the unit tests.
//! `cargo xtask test --tier bench -p marlin-jobs`, or
//! `cargo bench -p marlin-jobs --bench jobs -- [filter]`.

#[path = "../tests/support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicUsize, Ordering};

use marlin_bench::Bench;
use marlin_jobs::JobSystem;
use marlin_std::mem::MarlinHeap;

use support::system;

#[global_allocator]
static HEAP: MarlinHeap = MarlinHeap::new();

const EMPTY_JOBS: usize = 200_000;
/// 4096 leaves of 64 numbers: 4095 jobs wait on their children.
const TREE: u64 = 64 * 4096;

/// Sums `lo..hi` by splitting in half and waiting on the halves from inside the job, allocating
/// on both sides of each wait (the unit tests' `jobs::tree_sum`).
#[allow(clippy::useless_vec)]
fn tree_sum(jobs: &JobSystem, lo: u64, hi: u64) -> u64 {
    if hi - lo <= 64 {
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

fn empty_jobs(jobs: &JobSystem) {
    let counter = AtomicUsize::new(0);
    jobs.scope(|s| {
        for _ in 0..EMPTY_JOBS {
            s.spawn(|| {
                counter.fetch_add(1, Ordering::Relaxed);
            });
        }
    });
    assert_eq!(counter.into_inner(), EMPTY_JOBS);
}

fn overhead() {
    let jobs = system(0, 512, 512);
    let workers = jobs.worker_count() as f64;

    Bench::new("empty_jobs")
        .throughput_items(EMPTY_JOBS as u64, "job")
        .metric("workers", workers)
        .run(|| empty_jobs(&jobs));

    Bench::new("fork_join_tree")
        .throughput_items(TREE / 64 - 1, "wait")
        .run(|| {
            let mut result = 0;
            jobs.scope(|s| s.spawn(|| result = tree_sum(&jobs, 0, TREE)));
            assert_eq!(result, TREE * (TREE - 1) / 2);
        });

    // This thread queues into its own local queue and runs jobs while it waits.
    let main = jobs.main_thread();
    Bench::new("empty_jobs/main_registered")
        .throughput_items(EMPTY_JOBS as u64, "job")
        .metric("workers", workers)
        .run(|| empty_jobs(&jobs));
    drop(main);
}

marlin_bench::main!(overhead);
