//! Read-only, cheat and requires-restart rules. One test: cheats and startup are global.

use marlin_config::{
    CvarBool, CvarI64, CvarString, Error, Source, finish_startup, set_cheats_enabled,
    startup_finished,
};

static RO: CvarI64 = CvarI64::new("policy.ro", 1, "").read_only();
static CHEAT: CvarBool = CvarBool::new("policy.cheat", false, "").cheat();
static RESTART: CvarI64 = CvarI64::new("policy.restart", 1, "").requires_restart();
static RESTART_S: CvarString = CvarString::new("policy.restart_s", "a", "").requires_restart();

#[test]
fn policy() {
    // Read-only: code and the command line only.
    assert_eq!(RO.set_str("2", Source::Console), Err(Error::ReadOnly));
    assert_eq!(RO.set_str("2", Source::ConfigFile), Err(Error::ReadOnly));
    assert_eq!(RO.reset(Source::Console), Err(Error::ReadOnly));
    RO.set_str("3", Source::CommandLine).unwrap();
    assert_eq!(RO.get(), 3);
    RO.set(4);
    assert_eq!(RO.get(), 4);

    // Cheat: outside code only while cheats are enabled.
    for source in [Source::Console, Source::ConfigFile, Source::CommandLine] {
        assert_eq!(CHEAT.set_str("1", source), Err(Error::Cheat));
    }
    CHEAT.set_str("1", Source::Code).unwrap();
    set_cheats_enabled(true);
    CHEAT.set_str("0", Source::Console).unwrap();
    assert!(!CHEAT.get());
    set_cheats_enabled(false);

    // Requires restart: immediate during startup...
    assert!(!startup_finished());
    RESTART.set_str("2", Source::ConfigFile).unwrap();
    assert_eq!(RESTART.get(), 2);
    assert!(!RESTART.needs_restart());

    // ...pending afterwards, for code too.
    finish_startup();
    let generation = RESTART.generation();
    RESTART.set_str("5", Source::Console).unwrap();
    assert_eq!(RESTART.get(), 2);
    assert!(RESTART.needs_restart());
    assert_eq!(RESTART.pending_string().as_deref(), Some("5"));
    RESTART.set(7);
    assert_eq!(RESTART.pending_string().as_deref(), Some("7"));
    assert_eq!(
        RESTART.generation(),
        generation,
        "the live value never changed"
    );
    // Setting the live value again cancels the pending one.
    RESTART.set(2);
    assert!(!RESTART.needs_restart());
    assert_eq!(RESTART.pending_string(), None);

    RESTART_S.set("b");
    assert_eq!(RESTART_S.get(), "a");
    assert_eq!(RESTART_S.pending_string().as_deref(), Some("b"));
    RESTART_S.set("a");
    assert!(!RESTART_S.needs_restart());
}
