//! Per-target level rules (quick tier, docs/design/0014-testing.md §5). The filters are global,
//! so this binary holds a single test that changes them. No hardware needed.
//! Run alone with `cargo test -p marlin-diag --test isolated_log_target_filter`.

mod support;

use marlin_diag::{
    Level, LevelFilter, MAX_TARGET_LEVELS, STATIC_MAX_LEVEL, TargetTableFull, enabled_for,
};
use support::{CAPTURE, setup};

mod server {
    pub fn log() {
        marlin_diag::info!("server info");
        marlin_diag::debug!("server debug");
    }

    pub mod detail {
        pub fn log() {
            marlin_diag::info!("detail info");
            marlin_diag::debug!("detail debug");
        }
    }
}

mod servers {
    pub fn log() {
        marlin_diag::info!("servers info");
        marlin_diag::debug!("servers debug");
    }
}

fn log_all() -> Vec<String> {
    marlin_diag::info!("root info");
    server::log();
    server::detail::log();
    servers::log();
    CAPTURE.messages_current_thread()
}

#[test]
fn target_levels() {
    setup();
    let debug = STATIC_MAX_LEVEL.allows(Level::Debug);
    let with_debug = |names: &[&'static str]| -> Vec<String> {
        names
            .iter()
            .filter(|name| debug || !name.ends_with("debug"))
            .map(|name| name.to_string())
            .collect()
    };

    // No rules: the global filter alone.
    assert_eq!(
        log_all(),
        ["root info", "server info", "detail info", "servers info"]
    );

    // A rule more verbose than the global filter gets past the macros' early check, and covers
    // submodules but not a module that merely shares the name's prefix.
    marlin_diag::set_target_level("isolated_log_target_filter::server", LevelFilter::Debug)
        .unwrap();
    assert_eq!(
        log_all(),
        with_debug(&[
            "root info",
            "server info",
            "server debug",
            "detail info",
            "detail debug",
            "servers info",
        ])
    );

    // The longest prefix wins, in both directions.
    marlin_diag::set_target_level(
        "isolated_log_target_filter::server::detail",
        LevelFilter::Off,
    )
    .unwrap();
    marlin_diag::set_max_level(LevelFilter::Error);
    assert_eq!(log_all(), with_debug(&["server info", "server debug"]));

    // Setting the same prefix replaces its rule.
    marlin_diag::set_target_level("isolated_log_target_filter::server", LevelFilter::Info).unwrap();
    assert_eq!(log_all(), ["server info"]);

    // A partial component never matches.
    marlin_diag::set_target_level("isolated_log_target_filter::serv", LevelFilter::Trace).unwrap();
    assert!(!enabled_for(
        Level::Info,
        "isolated_log_target_filter::servers"
    ));
    assert!(enabled_for(Level::Info, "isolated_log_target_filter::serv"));
    assert!(enabled_for(
        Level::Info,
        "isolated_log_target_filter::serv::x"
    ));

    // Clearing.
    assert!(marlin_diag::clear_target_level(
        "isolated_log_target_filter::serv"
    ));
    assert!(!marlin_diag::clear_target_level(
        "isolated_log_target_filter::serv"
    ));
    assert!(marlin_diag::clear_target_level(
        "isolated_log_target_filter::server::detail"
    ));
    marlin_diag::set_max_level(LevelFilter::Info);
    assert_eq!(
        log_all(),
        ["root info", "server info", "detail info", "servers info"]
    );
    assert!(marlin_diag::clear_target_level(
        "isolated_log_target_filter::server"
    ));

    // The table is bounded.
    const PREFIXES: [&str; MAX_TARGET_LEVELS + 1] = [
        "p0", "p1", "p2", "p3", "p4", "p5", "p6", "p7", "p8", "p9", "p10", "p11", "p12", "p13",
        "p14", "p15", "p16",
    ];
    for prefix in &PREFIXES[..MAX_TARGET_LEVELS] {
        marlin_diag::set_target_level(prefix, LevelFilter::Off).unwrap();
    }
    assert_eq!(
        marlin_diag::set_target_level(PREFIXES[MAX_TARGET_LEVELS], LevelFilter::Off),
        Err(TargetTableFull)
    );
    assert!(!enabled_for(Level::Error, "p7::x"));
    assert!(enabled_for(Level::Error, "p77"));
    for prefix in &PREFIXES[..MAX_TARGET_LEVELS] {
        assert!(marlin_diag::clear_target_level(prefix));
    }
    assert_eq!(log_all().len(), 4);
}
