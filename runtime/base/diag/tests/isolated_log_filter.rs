//! The run-time level filter (quick tier, docs/design/0014-testing.md §5). The filter is global,
//! so this binary holds a single test that changes it. No hardware needed.
//! Run alone with `cargo test -p marlin-diag --test isolated_log_filter`.

mod support;

use marlin_diag::{Level, LevelFilter, STATIC_MAX_LEVEL};
use support::{CAPTURE, setup};

fn log_all() {
    marlin_diag::error!("e");
    marlin_diag::warn!("w");
    marlin_diag::info!("i");
    marlin_diag::debug!("d");
    marlin_diag::trace!("t");
}

fn expected(filter: LevelFilter) -> Vec<String> {
    [
        (Level::Error, "e"),
        (Level::Warn, "w"),
        (Level::Info, "i"),
        (Level::Debug, "d"),
        (Level::Trace, "t"),
    ]
    .into_iter()
    .filter(|&(level, _)| filter.allows(level) && STATIC_MAX_LEVEL.allows(level))
    .map(|(_, m)| m.to_owned())
    .collect()
}

#[test]
fn filtering() {
    setup();

    // Default: Info.
    assert_eq!(marlin_diag::max_level(), LevelFilter::Info);
    log_all();
    assert_eq!(
        CAPTURE.messages_current_thread(),
        expected(LevelFilter::Info)
    );

    for filter in [
        LevelFilter::Off,
        LevelFilter::Error,
        LevelFilter::Warn,
        LevelFilter::Info,
        LevelFilter::Debug,
        LevelFilter::Trace,
    ] {
        marlin_diag::set_max_level(filter);
        assert_eq!(marlin_diag::max_level(), filter.min(STATIC_MAX_LEVEL));
        log_all();
        assert_eq!(
            CAPTURE.messages_current_thread(),
            expected(filter),
            "{filter:?}"
        );
    }

    // `log!` takes a run-time level and obeys the same filters.
    marlin_diag::set_max_level(LevelFilter::Warn);
    for level in [Level::Error, Level::Warn, Level::Info, Level::Trace] {
        marlin_diag::log!(level, "{level}");
    }
    assert_eq!(CAPTURE.messages_current_thread(), ["ERROR", "WARN"]);
    assert!(marlin_diag::enabled(Level::Warn));
    assert!(!marlin_diag::enabled(Level::Info));
}

#[test]
fn level_order() {
    assert!(Level::Error < Level::Warn && Level::Debug < Level::Trace);
    assert!(LevelFilter::Info.allows(Level::Warn));
    assert!(!LevelFilter::Info.allows(Level::Debug));
    assert!(!LevelFilter::Off.allows(Level::Error));
    assert_eq!(LevelFilter::from(Level::Debug), LevelFilter::Debug);
    assert_eq!(format!("[{:<5}]", Level::Warn), "[WARN ]");
}
