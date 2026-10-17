use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use marlin_diag::{AsyncSink, Level, LineSink, Record, Sink};

/// Collects lines, optionally holding the worker inside the first write until released.
struct Collect {
    lines: Mutex<Vec<(Level, String, u64)>>,
    hold: AtomicBool,
    entered: AtomicBool,
    flushes: AtomicUsize,
}

impl Collect {
    fn leak(hold: bool) -> &'static Self {
        Box::leak(Box::new(Self {
            lines: Mutex::new(Vec::new()),
            hold: AtomicBool::new(hold),
            entered: AtomicBool::new(false),
            flushes: AtomicUsize::new(0),
        }))
    }

    fn take(&self) -> Vec<(Level, String, u64)> {
        std::mem::take(&mut *self.lines.lock().unwrap())
    }
}

impl LineSink for Collect {
    fn write_line(&self, level: Level, line: &str) {
        self.entered.store(true, Ordering::SeqCst);
        // Bounded: the test releases it within its own bounded steps.
        let mut spins = 0u32;
        while self.hold.load(Ordering::SeqCst) && spins < 5_000_000 {
            std::thread::yield_now();
            spins += 1;
        }
        self.lines
            .lock()
            .unwrap()
            .push((level, line.to_owned(), marlin_pal::thread::current_id()));
    }

    fn flush(&self) {
        self.flushes.fetch_add(1, Ordering::SeqCst);
    }
}

fn log(sink: &AsyncSink, level: Level, thread: u64, args: std::fmt::Arguments<'_>) {
    sink.log(&Record::new(level, "async_sink", file!(), 1, thread, args).with_timestamp(0));
}

#[test]
fn delivers_everything_in_order_per_thread() {
    let out = Collect::leak(false);
    let sink = AsyncSink::new(out, 64 * 1024).unwrap();
    const THREADS: u64 = 4;
    const RECORDS: u32 = 500;
    std::thread::scope(|scope| {
        for t in 0..THREADS {
            let sink = &sink;
            scope.spawn(move || {
                for i in 0..RECORDS {
                    log(sink, Level::Info, t, format_args!("{i}"));
                    // Keep the ring from filling: flush now and then.
                    if i % 100 == 99 {
                        Sink::flush(sink);
                    }
                }
            });
        }
    });
    Sink::flush(&sink);
    assert!(out.flushes.load(Ordering::SeqCst) > 0);
    assert_eq!(sink.dropped(), 0);

    let lines = out.take();
    assert_eq!(lines.len(), (THREADS * RECORDS as u64) as usize);
    let worker = lines[0].2;
    assert_ne!(worker, marlin_pal::thread::current_id());
    for t in 0..THREADS {
        let prefix = format!("[   0.000] INFO  [{t}] async_sink: ");
        let mine: Vec<u32> = lines
            .iter()
            .filter_map(|(_, line, _)| line.strip_prefix(&prefix))
            .map(|rest| rest.trim_end().parse().unwrap())
            .collect();
        assert_eq!(mine, (0..RECORDS).collect::<Vec<_>>());
    }
    assert!(lines.iter().all(|(level, line, thread)| {
        *level == Level::Info && line.ends_with('\n') && *thread == worker
    }));
}

#[test]
fn drops_and_counts_when_full() {
    let out = Collect::leak(true);
    // Room for a handful of ~40-byte records.
    let sink = AsyncSink::new(out, 256).unwrap();
    log(&sink, Level::Warn, 1, format_args!("first"));
    let mut spins = 0;
    while !out.entered.load(Ordering::SeqCst) {
        assert!(spins < 10_000_000, "worker never started");
        std::thread::yield_now();
        spins += 1;
    }
    // The worker is stuck in the first write; the ring fills up.
    const MORE: u64 = 100;
    for i in 0..MORE {
        log(&sink, Level::Error, 1, format_args!("{i}"));
    }
    let dropped = sink.dropped();
    assert!(dropped >= MORE - 8, "{dropped}");
    out.hold.store(false, Ordering::SeqCst);
    Sink::flush(&sink);
    let lines = out.take();
    assert_eq!(lines.len() as u64 + dropped, MORE + 1);
    assert!(lines[0].1.ends_with("first\n"));
    assert_eq!(lines[0].0, Level::Warn);
    // What was kept is the oldest, in order.
    for (i, (level, line, _)) in lines[1..].iter().enumerate() {
        assert_eq!(*level, Level::Error);
        assert!(line.ends_with(&format!(": {i}\n")), "{line}");
    }

    // After `stop`, records go straight to the destination, on the calling thread.
    sink.stop();
    sink.stop();
    log(&sink, Level::Info, 1, format_args!("direct"));
    let lines = out.take();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].2, marlin_pal::thread::current_id());
    Sink::flush(&sink);
}

#[test]
fn drop_stops_the_worker() {
    let out = Collect::leak(false);
    let sink = AsyncSink::new(out, 4096).unwrap();
    for i in 0..10 {
        log(&sink, Level::Info, 1, format_args!("{i}"));
    }
    drop(sink);
    // Stopping drains the ring first.
    assert_eq!(out.take().len(), 10);
}
