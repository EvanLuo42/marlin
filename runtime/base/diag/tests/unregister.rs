//! Mutates the global registry, so this binary holds a single test.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use common::Capture;
use marlin_diag::{Level, MAX_SINKS, Record, Sink};

static STEADY: Capture = Capture::new();
static CHURN: [Capture; 3] = [const { Capture::new() }; 3];
static FILL: [Capture; MAX_SINKS] = [const { Capture::new() }; MAX_SINKS];

/// Counts records without keeping them.
struct Counter(AtomicUsize);

impl Sink for Counter {
    fn log(&self, _: &Record<'_>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

static COUNTER: Counter = Counter(AtomicUsize::new(0));

#[test]
fn unregister() {
    // Not registered: nothing to remove.
    assert!(!marlin_diag::unregister_sink(&STEADY));

    marlin_diag::register_sink(&STEADY).unwrap();
    marlin_diag::register_sink(&COUNTER).unwrap();

    // Loggers run while sinks come and go; the steady sink sees every record.
    const THREADS: usize = 4;
    const RECORDS: usize = 2000;
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for t in 0..THREADS {
            scope.spawn(move || {
                for i in 0..RECORDS {
                    marlin_diag::error!("{t} {i}");
                }
            });
        }
        let stop = &stop;
        scope.spawn(move || {
            let mut rounds = 0;
            while !stop.load(Ordering::Relaxed) && rounds < 10_000 {
                for sink in &CHURN {
                    marlin_diag::register_sink(sink).unwrap();
                }
                for sink in CHURN.iter().rev() {
                    assert!(marlin_diag::unregister_sink(sink));
                }
                rounds += 1;
            }
        });
        // Scoped threads are joined at the end; stop the churn once the loggers are done.
        while COUNTER.0.load(Ordering::Relaxed) < THREADS * RECORDS {
            std::thread::yield_now();
        }
        stop.store(true, Ordering::Relaxed);
    });

    let all = STEADY.take_all();
    assert_eq!(all.len(), THREADS * RECORDS);
    for sink in &CHURN {
        let got = sink.take_all();
        assert!(got.len() <= THREADS * RECORDS);
    }

    // Once removed, a sink gets no more records.
    assert!(marlin_diag::unregister_sink(&COUNTER));
    assert!(!marlin_diag::unregister_sink(&COUNTER));
    let before = COUNTER.0.load(Ordering::Relaxed);
    marlin_diag::error!("after");
    assert_eq!(COUNTER.0.load(Ordering::Relaxed), before);
    assert_eq!(STEADY.messages_current_thread(), ["after"]);

    // Freed slots are reused.
    assert!(marlin_diag::unregister_sink(&STEADY));
    for sink in &FILL {
        marlin_diag::register_sink(sink).unwrap();
    }
    assert!(marlin_diag::register_sink(&STEADY).is_err());
    assert!(marlin_diag::unregister_sink(&FILL[3]));
    marlin_diag::register_sink(&STEADY).unwrap();
    marlin_diag::dispatch(&Record::new(
        Level::Info,
        "unregister",
        file!(),
        line!(),
        marlin_pal::thread::current_id(),
        format_args!("full"),
    ));
    assert_eq!(STEADY.messages_current_thread(), ["full"]);
    assert!(FILL[3].messages_current_thread().is_empty());
    assert_eq!(FILL[0].messages_current_thread(), ["full"]);

    // With every sink removed, records fall back to `DebugOutput` again.
    for (i, sink) in FILL.iter().enumerate() {
        assert_eq!(marlin_diag::unregister_sink(sink), i != 3);
    }
    assert!(marlin_diag::unregister_sink(&STEADY));
    marlin_diag::error!("to the debug output");
    assert!(STEADY.take_all().is_empty());
}
