mod common;

use std::panic::{self, AssertUnwindSafe};
use std::sync::Once;

use common::Capture;
use marlin_diag::{Level, LevelFilter, OrFatal, chain, fatal, marlin_verify, take_fatal_report};
use marlin_error::{Error, ErrorKind};

static CAPTURE: Capture = Capture::new();

fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| marlin_diag::register_sink(&CAPTURE).unwrap());
}

fn panic_message(f: impl FnOnce()) -> String {
    let payload = panic::catch_unwind(AssertUnwindSafe(f)).unwrap_err();
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ErrorKind)]
enum Kind {
    #[error("not found")]
    NotFound,
}

#[derive(Clone, Copy, Debug, Error)]
#[error(code = "os error {}")]
struct OsError {
    kind: Kind,
    raw: i32,
}

#[derive(Debug, Error)]
enum Load {
    #[error("reading {0}")]
    Read(&'static str, #[source] OsError),
}

#[derive(Debug, Error)]
enum Failure {
    #[error("assets")]
    Assets(#[from] Load),
}

fn failure() -> Failure {
    Failure::from(Load::Read(
        "a.pak",
        OsError {
            kind: Kind::NotFound,
            raw: 2,
        },
    ))
}

#[test]
fn chain_prints_every_source() {
    assert_eq!(
        chain(&failure()).to_string(),
        "assets: reading a.pak: not found (os error 2)"
    );
    let leaf = OsError {
        kind: Kind::NotFound,
        raw: 0,
    };
    assert_eq!(chain(&leaf).to_string(), "not found");
}

#[test]
fn or_fatal_logs_the_chain_once_then_panics() {
    setup();
    // Fatal errors bypass the run-time filter.
    marlin_diag::set_max_level(LevelFilter::Off);
    let result: Result<(), Failure> = Err(failure());
    let line = line!() + 1;
    let message = panic_message(|| result.or_fatal("starting"));
    assert_eq!(
        message,
        "starting: assets: reading a.pak: not found (os error 2)"
    );

    let records = CAPTURE.take_current_thread();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].level, Level::Error);
    assert_eq!(records[0].line, line);
    assert!(
        records[0].message.starts_with(&message),
        "{}",
        records[0].message
    );
    assert!(
        records[0].message.contains(" at "),
        "{}",
        records[0].message
    );

    // The panic handler sees it was reported, once.
    assert!(take_fatal_report());
    assert!(!take_fatal_report());
}

#[test]
fn or_fatal_passes_values_through() {
    let ok: Result<u32, OsError> = Ok(4);
    assert_eq!(ok.or_fatal("never"), 4);
    assert!(!take_fatal_report());
}

#[test]
fn fatal_macro_reports_at_the_call_site() {
    setup();
    let line = line!() + 1;
    let message = panic_message(|| fatal!("gave up after {} tries", 3));
    assert_eq!(message, "gave up after 3 tries");
    let records = CAPTURE.take_current_thread();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].line, line);
    assert_eq!(records[0].target, module_path!());
    assert!(take_fatal_report());
}

#[test]
fn failed_assertions_are_marked_reported() {
    setup();
    let _ = panic_message(|| marlin_verify!(1 > 2));
    assert_eq!(CAPTURE.take_current_thread().len(), 1);
    assert!(take_fatal_report());
}

#[test]
fn ordinary_panics_are_not_marked() {
    let _ = panic_message(|| panic!("plain"));
    assert!(!take_fatal_report());
}
