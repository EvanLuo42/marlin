//! Recording from several std threads, short-lived threads, and a zone ended on another thread.
//!
//! A process of its own (quick tier, docs/design/0014-testing.md §5): the profiler is global.
//! Run alone with `cargo test -p marlin-profile --test isolated_threads`.
#![cfg(not(marlin_build = "shipping"))]

use std::collections::HashSet;
use std::sync::Barrier;
use std::thread;

use marlin_profile::{
    Zone, ZoneSite, clear, frame_mark, profile_scope, set_thread_name, with_history,
};

const THREADS: usize = 8;
const ITERATIONS: usize = 2000;

fn work(i: usize) {
    profile_scope!("outer");
    for _ in 0..3 {
        profile_scope!("inner");
        std::hint::black_box(i);
    }
}

#[test]
fn threads() {
    clear();

    // Concurrent recording; each thread's zones stay on its own track.
    let barrier = Barrier::new(THREADS);
    thread::scope(|s| {
        for t in 0..THREADS {
            let barrier = &barrier;
            s.spawn(move || {
                set_thread_name(&format!("worker {t}"));
                barrier.wait();
                for i in 0..ITERATIONS {
                    work(i);
                }
            });
        }
    });
    frame_mark();
    with_history(|h| {
        let frame = h.last().unwrap();
        assert_eq!(frame.zones().len(), THREADS * ITERATIONS * 4);
        let threads: HashSet<_> = frame.zones().iter().map(|z| z.thread()).collect();
        assert_eq!(threads.len(), THREADS);
        let names: HashSet<_> = threads
            .iter()
            .map(|&t| h.thread(t).unwrap().name().unwrap().to_owned())
            .collect();
        assert_eq!(names.len(), THREADS);
        for z in frame.zones() {
            assert!(!z.suspended());
            match z.name() {
                "outer" => assert!(z.parent().is_none()),
                "inner" => assert_eq!(frame.zones()[z.parent().unwrap()].name(), "outer"),
                other => panic!("unexpected zone {other}"),
            }
        }
        let stats = h.zone_stats();
        let inner = stats.iter().find(|s| s.name == "inner").unwrap();
        assert_eq!(inner.calls, (THREADS * ITERATIONS * 3) as u64);
        assert_eq!(h.dropped_events(), 0);
    });

    // Many short-lived threads: exited threads' buffers are reused, and each thread still gets
    // its own record.
    for round in 0..4 {
        for _ in 0..32 {
            thread::spawn(|| work(0)).join().unwrap();
        }
        frame_mark();
        with_history(|h| {
            let frame = h.last().unwrap();
            assert_eq!(frame.zones().len(), 32 * 4, "round {round}");
            let threads: HashSet<_> = frame.zones().iter().map(|z| z.thread()).collect();
            assert_eq!(threads.len(), 32);
        });
    }

    // A zone moved to another thread ends there: paired by id, attributed to where it began.
    static MOVED: ZoneSite = ZoneSite {
        name: "moved",
        file: file!(),
        line: line!(),
    };
    let zone = Zone::begin(&MOVED);
    thread::spawn(move || drop(zone)).join().unwrap();
    frame_mark();
    with_history(|h| {
        let frame = h.last().unwrap();
        assert_eq!(frame.zones().len(), 1);
        let z = &frame.zones()[0];
        assert_eq!(z.name(), "moved");
        assert_ne!(z.thread(), z.end_thread());
        assert!(z.suspended());
    });
}
