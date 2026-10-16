mod common;

use std::sync::Once;

use common::Capture;
use marlin_diag::{DebugOutput, Level, Record, Sink};

static FIRST: Capture = Capture::new();
static SECOND: Capture = Capture::new();

fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        marlin_diag::register_sink(&FIRST).unwrap();
        marlin_diag::register_sink(&SECOND).unwrap();
    });
}

#[test]
fn records_carry_metadata() {
    setup();
    let answer = 42;
    let line = line!() + 1;
    marlin_diag::warn!("answer = {answer}, {:>4}", "pad");
    let records = FIRST.take_current_thread();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.level, Level::Warn);
    assert_eq!(record.message, "answer = 42,  pad");
    assert_eq!(record.target, module_path!());
    assert_eq!(record.file, file!());
    assert_eq!(record.line, line);
    assert_eq!(record.thread_id, marlin_pal::thread::current_id());
}

#[test]
fn every_sink_receives_records() {
    setup();
    marlin_diag::error!("to all");
    assert_eq!(FIRST.messages_current_thread(), ["to all"]);
    assert_eq!(SECOND.messages_current_thread(), ["to all"]);
}

#[test]
fn records_from_other_threads() {
    setup();
    let id = std::thread::spawn(|| {
        marlin_diag::error!("from a thread");
        let records = FIRST.take_current_thread();
        SECOND.take_current_thread();
        assert_eq!(records.len(), 1);
        records[0].thread_id
    })
    .join()
    .unwrap();
    assert_ne!(id, marlin_pal::thread::current_id());
}

#[test]
fn compiled_out_arguments_are_not_evaluated() {
    setup();
    marlin_diag::set_max_level(marlin_diag::LevelFilter::Info);
    let mut evaluated = false;
    marlin_diag::debug!("{}", {
        evaluated = true;
        1
    });
    assert!(!evaluated);
}

#[test]
fn debug_output_formats_without_panicking() {
    let long = "é".repeat(DebugOutput::LINE);
    DebugOutput.log(&Record::new(
        Level::Info,
        "sink",
        file!(),
        line!(),
        1,
        format_args!("{long}"),
    ));
    DebugOutput.flush();
}
