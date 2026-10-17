//! Helpers shared by marlin-jobs's test binaries.
#![allow(dead_code)]

use marlin_jobs::{Config, JobSystem};

/// The fiber stack size most tests use.
pub const STACK: usize = 128 << 10;

/// A job system with unpinned workers and [`STACK`]-sized fibers.
pub fn system(workers: usize, fibers: usize, max_fibers: usize) -> JobSystem {
    system_with_stack(workers, fibers, max_fibers, STACK)
}

pub fn system_with_stack(
    workers: usize,
    fibers: usize,
    max_fibers: usize,
    stack: usize,
) -> JobSystem {
    JobSystem::new(Config {
        workers,
        fibers,
        max_fibers,
        fiber_stack_size: stack,
        pin_workers: false,
    })
    .unwrap()
}

/// Sums `lo..hi` by splitting it into nested scopes down to 16 numbers: a deep tree of jobs that
/// wait on their children.
pub fn tree_sum(jobs: &JobSystem, lo: u64, hi: u64) -> u64 {
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
