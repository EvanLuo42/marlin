mod common;

use std::sync::Once;
use std::time::Duration;

use common::Capture;
use marlin_diag::{ConsoleSink, Level, LineSink, Record, Sink};

static CAPTURE: Capture = Capture::new();

fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        marlin_diag::init();
        marlin_diag::register_sink(&CAPTURE).unwrap();
    });
}

#[test]
fn timestamps_increase() {
    setup();
    for i in 0..50 {
        marlin_diag::error!("{i}");
    }
    std::thread::sleep(Duration::from_millis(5));
    marlin_diag::error!("later");
    let records = CAPTURE.take_current_thread();
    assert_eq!(records.len(), 51);
    assert!(records.windows(2).all(|w| w[0].timestamp <= w[1].timestamp));
    let (first, last) = (records[0].timestamp, records[50].timestamp);
    let tps = marlin_pal::time::ticks_per_second();
    assert!(
        (last - first) as u128 * 1000 >= 4 * tps as u128,
        "{first} -> {last}"
    );
}

#[test]
fn since_start_is_relative_to_init() {
    setup();
    let now = marlin_pal::time::ticks();
    let record = Record::new(Level::Info, "t", file!(), line!(), 1, format_args!("x"));
    assert!(record.timestamp() >= now);
    // The clock started in `setup`, before `now`.
    assert!(record.since_start() < Duration::from_secs(60));
    let early = record.with_timestamp(0);
    assert_eq!(early.timestamp(), 0);
    assert_eq!(early.since_start(), Duration::ZERO);
}

#[test]
fn display_format() {
    let n = 3;
    let line = Record::new(
        Level::Warn,
        "a::b",
        file!(),
        line!(),
        7,
        format_args!("hi {n}"),
    )
    .with_timestamp(0)
    .to_string();
    assert_eq!(line, "[   0.000] WARN  [7] a::b: hi 3");
}

#[test]
fn console_sink_formats_without_panicking() {
    for color in [false, true] {
        let sink = ConsoleSink::new(color);
        sink.log(&Record::new(
            Level::Error,
            "timestamp",
            file!(),
            line!(),
            1,
            format_args!("message"),
        ));
        sink.write_line(Level::Info, "no level here\n");
        Sink::flush(&sink);
    }
}
