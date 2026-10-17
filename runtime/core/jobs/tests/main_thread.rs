//! The main thread as a worker, and jobs that only it runs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use marlin_jobs::{Config, JobHandle, JobSystem};
use marlin_std::mem::MarlinHeap;
use marlin_std::thread::{ThreadId, current};

#[global_allocator]
static HEAP: MarlinHeap = MarlinHeap::new();

fn system(workers: usize) -> JobSystem {
    JobSystem::new(Config {
        workers,
        fibers: 32,
        max_fibers: 512,
        fiber_stack_size: 128 << 10,
        pin_workers: false,
    })
    .unwrap()
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
fn main_thread_runs_jobs_while_it_waits() {
    let jobs = system(2);
    let main = jobs.main_thread();
    let me = current::id();
    let on_main = AtomicUsize::new(0);
    // Jobs spawned from here, waited on here: some run on this thread.
    for _ in 0..20 {
        jobs.parallel_for(0..2000, 1, |_| {
            if current::id() == me {
                on_main.fetch_add(1, Ordering::Relaxed);
            }
            std::hint::black_box(());
        });
    }
    let n = 16 * 1024;
    let mut sum = 0;
    jobs.scope(|s| s.spawn(|| sum = tree_sum(&jobs, 0, n)));
    assert_eq!(sum, n * (n - 1) / 2);
    println!(
        "main thread ran {} of 40000 jobs",
        on_main.load(Ordering::Relaxed)
    );
    drop(main);
}

#[test]
fn main_thread_jobs_run_only_on_the_main_thread() {
    let jobs = system(3);
    let main = jobs.main_thread();
    let me = current::id();
    let ran = Arc::new(AtomicUsize::new(0));
    let job = |ran: &Arc<AtomicUsize>| {
        let ran = ran.clone();
        move || {
            assert_eq!(current::id(), me);
            ran.fetch_add(1, Ordering::Relaxed);
        }
    };
    // From here, and from worker jobs.
    let mut handles: Vec<JobHandle> = (0..50).map(|_| jobs.spawn_on_main(job(&ran))).collect();
    let from_workers = std::sync::Mutex::new(Vec::new());
    jobs.scope(|s| {
        for _ in 0..50 {
            s.spawn(|| {
                let h = jobs.spawn_on_main(job(&ran));
                from_workers.lock().unwrap().push(h);
            });
        }
    });
    handles.extend(from_workers.into_inner().unwrap());
    for h in &handles {
        main.run_until(h);
    }
    assert_eq!(ran.load(Ordering::Relaxed), 100);
}

#[test]
fn main_thread_jobs_stay_on_it_through_waits() {
    let jobs = system(3);
    let main = jobs.main_thread();
    let me = current::id();
    let waits = AtomicUsize::new(0);
    jobs.scope(|s| {
        for _ in 0..4 {
            s.spawn_on_main(|| {
                for _ in 0..10 {
                    jobs.scope(|inner| {
                        for _ in 0..8 {
                            inner.spawn(|| std::thread::sleep(Duration::from_micros(50)));
                        }
                    });
                    assert_eq!(current::id(), me, "a main-thread job left the main thread");
                    waits.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    assert_eq!(waits.load(Ordering::Relaxed), 40);
    drop(main);
}

#[test]
fn scoped_main_thread_jobs() {
    let jobs = system(2);
    let main = jobs.main_thread();
    let me = current::id();
    let mut hits = vec![0u32; 64];
    jobs.scope(|s| {
        for h in hits.iter_mut() {
            s.spawn_on_main(move || {
                assert_eq!(current::id(), me);
                *h += 1;
            });
        }
    });
    assert!(hits.iter().all(|&h| h == 1));
    drop(main);
}

#[test]
fn pump_runs_what_is_ready() {
    let jobs = system(1);
    let main = jobs.main_thread();
    let me = current::id();
    let ran = Arc::new(AtomicUsize::new(0));
    let handles: Vec<JobHandle> = (0..10)
        .map(|_| {
            let ran = ran.clone();
            jobs.spawn_on_main(move || {
                assert_eq!(current::id(), me);
                ran.fetch_add(1, Ordering::Relaxed);
            })
        })
        .collect();
    // Nothing runs main-thread jobs until the main thread pumps.
    std::thread::sleep(Duration::from_millis(10));
    assert_eq!(ran.load(Ordering::Relaxed), 0);
    assert_eq!(main.pump(3), 3);
    assert_eq!(ran.load(Ordering::Relaxed), 3);
    let mut total = 3;
    while total < 10 {
        total += main.pump(100);
    }
    assert_eq!(ran.load(Ordering::Relaxed), 10);
    assert!(handles.iter().all(JobHandle::is_complete));
    assert_eq!(main.pump(100), 0);
}

#[test]
fn pump_resumes_main_thread_jobs_after_their_waits() {
    let jobs = system(2);
    let main = jobs.main_thread();
    let me = current::id();
    let gate = Arc::new(AtomicBool::new(false));
    let g = gate.clone();
    let slow = jobs.spawn(move || {
        while !g.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_micros(100));
        }
    });
    let s = slow.clone();
    let h = jobs.spawn_on_main(move || {
        s.wait();
        assert_eq!(current::id(), me);
    });
    // The main-thread job starts, waits on `slow`; pump returns meanwhile.
    while main.pump(1) == 0 {}
    assert!(!h.is_complete());
    gate.store(true, Ordering::Release);
    slow.wait();
    while !h.is_complete() {
        main.pump(10);
    }
}

#[test]
fn registration_is_exclusive() {
    let jobs = system(1);
    let main = jobs.main_thread();
    std::thread::scope(|t| {
        t.spawn(|| {
            let other =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| jobs.main_thread()));
            assert!(other.is_err());
        });
    });
    drop(main);
    // Free again: another thread may take over.
    std::thread::scope(|t| {
        t.spawn(|| {
            let main = jobs.main_thread();
            main.run_until(&jobs.spawn_on_main(|| {}));
        });
    });
}

#[test]
fn dropping_the_system_runs_pending_main_thread_jobs() {
    let ran = Arc::new(AtomicUsize::new(0));
    let me: ThreadId = current::id();
    {
        let jobs = system(2);
        for _ in 0..10 {
            let ran = ran.clone();
            jobs.spawn_on_main(move || {
                assert_eq!(current::id(), me);
                ran.fetch_add(1, Ordering::Relaxed);
            });
        }
        let ran2 = ran.clone();
        let first = jobs.spawn(|| std::thread::sleep(Duration::from_millis(2)));
        jobs.spawn_on_main_after(&[&first], move || {
            ran2.fetch_add(1, Ordering::Relaxed);
        });
    }
    assert_eq!(ran.load(Ordering::Relaxed), 11);
}
