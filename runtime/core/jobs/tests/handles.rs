//! Detached jobs, handles and dependencies: the links of a frame pipeline.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use marlin_jobs::{Config, JobHandle, JobSystem, Priority};
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
fn spawned_jobs_complete_their_handles() {
    let jobs = system(4, 32);
    let sum = Arc::new(AtomicUsize::new(0));
    let handles: Vec<JobHandle> = (0..100)
        .map(|i| {
            let sum = sum.clone();
            jobs.spawn(move || {
                sum.fetch_add(i, Ordering::Relaxed);
            })
        })
        .collect();
    for h in &handles {
        h.wait();
        assert!(h.is_complete());
    }
    assert_eq!(sum.load(Ordering::Relaxed), (0..100).sum());
    assert!(JobHandle::complete().is_complete());
    assert!(JobHandle::default().is_complete());
    JobHandle::complete().wait();
}

#[test]
fn dependencies_order_jobs() {
    let jobs = system(4, 32);
    for _ in 0..200 {
        let log = Arc::new(Mutex::new(Vec::new()));
        let job = |name: &'static str, log: &Arc<Mutex<Vec<&'static str>>>| {
            let log = log.clone();
            move || {
                std::hint::spin_loop();
                log.lock().unwrap().push(name);
            }
        };
        // A diamond: a, then b and c in either order, then d.
        let a = jobs.spawn(job("a", &log));
        let b = jobs.spawn_after(&[&a], Priority::Normal, job("b", &log));
        let c = jobs.spawn_after(&[&a], Priority::High, job("c", &log));
        let d = jobs.spawn_after(&[&b, &c], Priority::Low, job("d", &log));
        d.wait();
        assert!(a.is_complete() && b.is_complete() && c.is_complete());
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 4);
        assert_eq!((log[0], log[3]), ("a", "d"), "{log:?}");
    }
}

#[test]
fn complete_dependencies_do_not_delay() {
    let jobs = system(2, 16);
    let a = jobs.spawn(|| {});
    a.wait();
    let ran = Arc::new(AtomicBool::new(false));
    let r = ran.clone();
    jobs.spawn_after(&[&a, &JobHandle::complete()], Priority::Normal, move || {
        r.store(true, Ordering::Release)
    })
    .wait();
    assert!(ran.load(Ordering::Acquire));
}

#[test]
fn waiting_dependents_hold_no_fibers() {
    // Far more dependents than fibers: they must wait as continuations, not parked fibers.
    let jobs = system(2, 8);
    let gate = Arc::new(AtomicBool::new(false));
    let g = gate.clone();
    let blocker = jobs.spawn(move || {
        while !g.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let count = Arc::new(AtomicU32::new(0));
    let dependents: Vec<JobHandle> = (0..1000)
        .map(|_| {
            let count = count.clone();
            jobs.spawn_after(&[&blocker], Priority::Normal, move || {
                count.fetch_add(1, Ordering::Relaxed);
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(count.load(Ordering::Relaxed), 0);
    assert!(dependents.iter().all(|d| !d.is_complete()));
    gate.store(true, Ordering::Release);
    dependents.iter().for_each(JobHandle::wait);
    assert_eq!(count.load(Ordering::Relaxed), 1000);
}

#[test]
fn batches_complete_when_all_their_jobs_do() {
    let jobs = system(4, 32);
    let count = Arc::new(AtomicU32::new(0));
    let first = jobs.spawn(|| std::thread::sleep(Duration::from_millis(5)));
    let batch = jobs.batch(|b| {
        for i in 0..64 {
            let count = count.clone();
            let job = move || {
                count.fetch_add(1, Ordering::Relaxed);
            };
            match i % 3 {
                0 => b.spawn(job),
                1 => b.spawn_with(Priority::High, job),
                _ => b.spawn_after(&[&first], Priority::Low, job),
            }
        }
    });
    batch.wait();
    assert_eq!(count.load(Ordering::Relaxed), 64);
    assert!(first.is_complete());
    // An empty batch is complete at once.
    assert!(jobs.batch(|_| {}).is_complete());
}

#[test]
fn jobs_wait_on_handles_by_parking() {
    let jobs = system(2, 64);
    let slow = jobs.spawn(|| std::thread::sleep(Duration::from_millis(20)));
    // More waiting jobs than workers: they park instead of blocking the workers.
    let waiters: Vec<JobHandle> = (0..16)
        .map(|_| {
            let slow = slow.clone();
            jobs.spawn(move || {
                slow.wait();
                assert!(slow.is_complete());
            })
        })
        .collect();
    waiters.iter().for_each(JobHandle::wait);
}

/// The frame pipeline: `extract(N)` after `sim(N)`, `render(N)` after
/// `extract(N)` and `render(N - 1)`, and `sim(N + 2)` after `render(N)`.
#[test]
fn frame_pipeline_keeps_its_order() {
    const FRAMES: usize = 60;
    let jobs = system(4, 64);
    #[derive(Default)]
    struct Stages {
        sim: Vec<AtomicBool>,
        extract: Vec<AtomicBool>,
        render: Vec<AtomicBool>,
    }
    let stages = Arc::new(Stages {
        sim: (0..FRAMES).map(|_| AtomicBool::new(false)).collect(),
        extract: (0..FRAMES).map(|_| AtomicBool::new(false)).collect(),
        render: (0..FRAMES).map(|_| AtomicBool::new(false)).collect(),
    });
    let done = |v: &Vec<AtomicBool>, i: usize| v[i].load(Ordering::Acquire);
    let mut renders: Vec<JobHandle> = Vec::new();
    let mut last_render = JobHandle::complete();
    for n in 0..FRAMES {
        // sim(N) after render(N - 2).
        let after = match n {
            0 | 1 => JobHandle::complete(),
            _ => renders[n - 2].clone(),
        };
        let s = stages.clone();
        let sim = jobs.spawn_after(&[&after], Priority::Normal, move || {
            assert!(
                n < 2 || done(&s.render, n - 2),
                "sim {n} before render {}",
                n - 2
            );
            s.sim[n].store(true, Ordering::Release);
        });
        let s = stages.clone();
        let extract = jobs.spawn_after(&[&sim], Priority::Normal, move || {
            assert!(done(&s.sim, n), "extract {n} before sim {n}");
            s.extract[n].store(true, Ordering::Release);
        });
        let s = stages.clone();
        let render = jobs.spawn_after(&[&extract, &last_render], Priority::High, move || {
            assert!(done(&s.extract, n), "render {n} before extract {n}");
            assert!(
                n == 0 || done(&s.render, n - 1),
                "render {n} before render {}",
                n - 1
            );
            s.render[n].store(true, Ordering::Release);
        });
        last_render = render.clone();
        renders.push(render);
    }
    last_render.wait();
    assert!(stages.render.iter().all(|r| r.load(Ordering::Acquire)));
}

#[test]
fn dropping_the_system_finishes_detached_work() {
    let ran = Arc::new(AtomicU32::new(0));
    {
        let jobs = system(2, 16);
        let r = ran.clone();
        let first = jobs.spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            r.fetch_add(1, Ordering::Relaxed);
        });
        let r = ran.clone();
        jobs.spawn_after(&[&first], Priority::Low, move || {
            r.fetch_add(1, Ordering::Relaxed);
        });
    }
    assert_eq!(ran.load(Ordering::Relaxed), 2);
}

#[test]
fn handles_cross_job_systems() {
    let a = system(1, 8);
    let b = system(1, 8);
    let flag = Arc::new(AtomicBool::new(false));
    let f = flag.clone();
    let first = a.spawn(move || {
        std::thread::sleep(Duration::from_millis(5));
        f.store(true, Ordering::Release);
    });
    let f = flag.clone();
    b.spawn_after(&[&first], Priority::Normal, move || {
        assert!(f.load(Ordering::Acquire))
    })
    .wait();
}
