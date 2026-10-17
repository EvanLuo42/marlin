//! Read-only, cheat, development-only, replicated and requires-restart rules. One test: these
//! switches and startup are global.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use marlin_config::{
    CvarBool, CvarI64, CvarString, DEV_ENABLED, Error, Source, finish_startup,
    on_replicated_change, register_all, remove_callback, replicated_locked, set_cheats_enabled,
    set_replicated_lock, startup_finished,
};

static RO: CvarI64 = CvarI64::new("policy.ro", 1, "").read_only();
static CHEAT: CvarBool = CvarBool::new("policy.cheat", false, "").cheat();
static DEV: CvarBool = CvarBool::new("policy.dev", false, "").dev_only();
static REPLICATED: CvarI64 = CvarI64::new("policy.gravity", 800, "").replicated();
static RESTART: CvarI64 = CvarI64::new("policy.restart", 1, "").requires_restart();
static RESTART_S: CvarString = CvarString::new("policy.restart_s", "a", "").requires_restart();

#[test]
fn policy() {
    // Read-only: code and the command line only.
    assert_eq!(RO.set_str("2", Source::Console), Err(Error::ReadOnly));
    assert_eq!(RO.set_str("2", Source::User), Err(Error::ReadOnly));
    assert_eq!(RO.reset(Source::Console), Err(Error::ReadOnly));
    RO.set_str("3", Source::CommandLine).unwrap();
    assert_eq!(RO.get(), 3);
    RO.set(4);
    assert_eq!(RO.get(), 4);

    // Cheat: outside code only while cheats are enabled.
    for source in [Source::Console, Source::User, Source::CommandLine] {
        assert_eq!(CHEAT.set_str("1", source), Err(Error::Cheat));
    }
    CHEAT.set_str("1", Source::Code).unwrap();
    set_cheats_enabled(true);
    CHEAT.set_str("0", Source::Console).unwrap();
    assert!(!CHEAT.get());
    set_cheats_enabled(false);

    // Development only: everywhere but shipping builds.
    assert_eq!(DEV.set_str("1", Source::Console).is_ok(), DEV_ENABLED);
    DEV.set(true);
    assert!(DEV.get());

    // Replicated: while connected, only the server and code set it; the hook sees every
    // change, whoever made it.
    register_all(&[&REPLICATED]).unwrap();
    let changes = Arc::new(AtomicUsize::new(0));
    let hook = on_replicated_change({
        let changes = changes.clone();
        move |c| {
            assert_eq!(c.name(), "policy.gravity");
            changes.fetch_add(1, Ordering::SeqCst);
        }
    });
    REPLICATED.set_str("600", Source::Console).unwrap();
    assert!(!replicated_locked());
    set_replicated_lock(true);
    assert_eq!(
        REPLICATED.set_str("1", Source::Console),
        Err(Error::Replicated)
    );
    assert_eq!(
        REPLICATED.set_str("1", Source::User),
        Err(Error::Replicated)
    );
    REPLICATED.set_str("400", Source::Server).unwrap();
    assert_eq!(
        (REPLICATED.get(), REPLICATED.set_by()),
        (400, Some(Source::Server))
    );
    // Disconnecting: dropping the server's layer brings the local value back.
    set_replicated_lock(false);
    REPLICATED.unset(Source::Server).unwrap();
    assert_eq!(
        (REPLICATED.get(), REPLICATED.set_by()),
        (600, Some(Source::Console))
    );
    DEV.set(false); // not replicated: no call
    assert_eq!(changes.load(Ordering::SeqCst), 3);
    assert!(remove_callback(hook));

    // Requires restart: immediate during startup...
    assert!(!startup_finished());
    RESTART.set_str("2", Source::User).unwrap();
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
