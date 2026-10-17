//! Cancelling detached jobs: skipped if not started, propagated to dependents.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use marlin_jobs::{JobHandle, JobSystem, Priority, is_cancelled};
use marlin_test_support::DropCounter;

use crate::support::system;

/// A job that runs until the returned gate opens, and the gate.
fn blocker(jobs: &JobSystem) -> (JobHandle, Arc<AtomicBool>, Arc<AtomicBool>) {
    let gate = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicBool::new(false));
    let (g, s) = (gate.clone(), started.clone());
    let h = jobs.spawn(move || {
        s.store(true, Ordering::Release);
        while !g.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_micros(200));
        }
    });
    (h, gate, started)
}

#[test]
fn cancelled_jobs_that_have_not_started_are_skipped() {
    let jobs = system(1, 16, 256);
    // The only worker is busy, so the jobs queued behind it have not started.
    let (busy, gate, started) = blocker(&jobs);
    while !started.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    let ran = Arc::new(AtomicU32::new(0));
    let dropped = DropCounter::new();
    let handles: Vec<JobHandle> = (0..10)
        .map(|_| {
            let (ran, guard) = (ran.clone(), dropped.token());
            jobs.spawn(move || {
                let _guard = guard;
                ran.fetch_add(1, Ordering::Relaxed);
            })
        })
        .collect();
    for h in handles.iter().step_by(2) {
        h.cancel();
    }
    gate.store(true, Ordering::Release);
    busy.wait();
    for (i, h) in handles.iter().enumerate() {
        h.wait();
        assert!(h.is_complete());
        assert_eq!(h.is_cancelled(), i % 2 == 0);
    }
    assert_eq!(ran.load(Ordering::Relaxed), 5);
    // Skipped closures are dropped, not leaked.
    assert_eq!(dropped.dropped(), 10);
    // Cancelling a complete or empty handle is harmless.
    handles[1].cancel();
    JobHandle::complete().cancel();
    assert!(!JobHandle::complete().is_cancelled());
}

#[test]
fn started_jobs_run_to_completion_and_can_check() {
    let jobs = system(2, 16, 256);
    let seen = Arc::new(AtomicBool::new(false));
    let go = Arc::new(AtomicBool::new(false));
    let (s, g) = (seen.clone(), go.clone());
    let h = jobs.spawn(move || {
        assert!(!is_cancelled());
        g.store(true, Ordering::Release);
        while !is_cancelled() {
            std::thread::sleep(Duration::from_micros(100));
        }
        s.store(true, Ordering::Release);
    });
    while !go.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    h.cancel();
    h.wait();
    assert!(seen.load(Ordering::Acquire));
    // Outside detached jobs there is nothing to cancel.
    assert!(!is_cancelled());
    jobs.scope(|s| s.spawn(|| assert!(!is_cancelled())));
}

#[test]
fn cancellation_propagates_to_dependents() {
    let jobs = system(2, 16, 256);
    let (first, gate, _) = blocker(&jobs);
    let ran = Arc::new(AtomicU32::new(0));
    let job = |ran: &Arc<AtomicU32>| {
        let ran = ran.clone();
        move || {
            ran.fetch_add(1, Ordering::Relaxed);
        }
    };
    let a = jobs.spawn_after(&[&first], Priority::Normal, job(&ran));
    let b = jobs.spawn_after(&[&a], Priority::High, job(&ran));
    let c = jobs.spawn_after(&[&b, &first], Priority::Low, job(&ran));
    // Not after `a`: unaffected.
    let d = jobs.spawn_after(&[&first], Priority::Normal, job(&ran));
    a.cancel();
    gate.store(true, Ordering::Release);
    for h in [&a, &b, &c, &d] {
        h.wait();
    }
    assert!(a.is_cancelled() && b.is_cancelled() && c.is_cancelled());
    assert!(!d.is_cancelled() && !first.is_cancelled());
    assert_eq!(ran.load(Ordering::Relaxed), 1);
    // Depending on an already cancelled handle skips at once.
    let e = jobs.spawn_after(&[&c], Priority::Normal, job(&ran));
    e.wait();
    assert!(e.is_cancelled());
    assert_eq!(ran.load(Ordering::Relaxed), 1);
}

#[test]
fn cancelling_a_batch_skips_its_unstarted_jobs() {
    let jobs = system(1, 16, 256);
    let (busy, gate, started) = blocker(&jobs);
    while !started.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    let ran = Arc::new(AtomicU32::new(0));
    let batch = jobs.batch(|b| {
        for _ in 0..20 {
            let ran = ran.clone();
            b.spawn(move || {
                ran.fetch_add(1, Ordering::Relaxed);
            });
        }
    });
    batch.cancel();
    gate.store(true, Ordering::Release);
    busy.wait();
    batch.wait();
    assert!(batch.is_complete() && batch.is_cancelled());
    assert_eq!(ran.load(Ordering::Relaxed), 0);
}

#[test]
fn dropping_the_system_with_cancelled_work() {
    let ran = Arc::new(AtomicU32::new(0));
    {
        let jobs = system(1, 16, 256);
        let (first, gate, _) = blocker(&jobs);
        for _ in 0..50 {
            let ran = ran.clone();
            jobs.spawn_after(&[&first], Priority::Normal, move || {
                ran.fetch_add(1, Ordering::Relaxed);
            });
        }
        first.cancel(); // Already running: it finishes; its dependents are skipped.
        gate.store(true, Ordering::Release);
    }
    assert_eq!(ran.load(Ordering::Relaxed), 0);
}
