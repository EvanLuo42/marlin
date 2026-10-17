//! Zones inside jobs that wait: the fiber may resume on another worker, so begin and end are
//! recorded on different threads and must still pair up.
//!
//! A process of its own (quick tier, docs/design/0014-testing.md §5): the profiler is global.
//! Run alone with `cargo test -p marlin-profile --test isolated_jobs`.
#![cfg(not(marlin_build = "shipping"))]

use std::collections::HashSet;

use marlin_jobs::{Config, JobSystem};
use marlin_profile::{clear, frame_mark, profile_scope, with_history};

/// Sums lo..hi by splitting and waiting on the halves inside a zone.
fn tree_sum(jobs: &JobSystem, lo: u64, hi: u64) -> u64 {
    profile_scope!("tree_sum");
    if hi - lo <= 16 {
        profile_scope!("leaf");
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
fn zones_across_job_waits() {
    let jobs = JobSystem::new(Config {
        workers: 4,
        fibers: 512,
        fiber_stack_size: 128 << 10,
        pin_workers: false,
        ..Config::default()
    })
    .unwrap();
    clear();

    let n = 16 * 256;
    let mut migrated = 0;
    for _ in 0..8 {
        let mut sum = 0;
        jobs.scope(|s| s.spawn(|| sum = tree_sum(&jobs, 0, n)));
        assert_eq!(sum, n * (n - 1) / 2);
        frame_mark();
        with_history(|h| {
            let frame = h.last().unwrap();
            // 256 leaves under a full binary tree of 511 `tree_sum` zones.
            let count = |name| frame.zones().iter().filter(|z| z.name() == name).count();
            assert_eq!(count("tree_sum"), 511);
            assert_eq!(count("leaf"), 256);
            let ids: HashSet<_> = frame.zones().iter().map(|z| z.id()).collect();
            assert_eq!(ids.len(), frame.zones().len());
            for z in frame.zones() {
                assert!(z.start_ticks() <= z.end_ticks());
                assert!(h.thread(z.thread()).is_some() && h.thread(z.end_thread()).is_some());
                if z.thread() != z.end_thread() {
                    assert!(z.suspended());
                    migrated += 1;
                }
                // Leaves never wait, so they always nest under their tree_sum.
                if z.name() == "leaf" {
                    assert!(!z.suspended());
                    assert_eq!(frame.zones()[z.parent().unwrap()].name(), "tree_sum");
                }
            }
            assert_eq!(h.dropped_events(), 0);
        });
    }
    // Not guaranteed on any one run, but 8 x 255 waits on 4 workers virtually always migrate.
    println!("{migrated} zones ended on another thread");

    with_history(|h| {
        let stats = h.zone_stats();
        let leaf = stats.iter().find(|s| s.name == "leaf").unwrap();
        assert_eq!(leaf.calls, 8 * 256);
        assert_eq!(leaf.frames, 8);
    });
}
