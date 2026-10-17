//! The bounded sink registry (quick tier, docs/design/0014-testing.md §5). It fills the global
//! registry, so this binary holds a single test. No hardware needed.
//! Run alone with `cargo test -p marlin-diag --test isolated_log_registry`.

mod support;

use marlin_diag::{MAX_SINKS, RegistryFull};
use support::Capture;

static SINKS: [Capture; MAX_SINKS + 1] = [const { Capture::new() }; MAX_SINKS + 1];

#[test]
fn registry_is_bounded() {
    for sink in &SINKS[..MAX_SINKS] {
        marlin_diag::register_sink(sink).unwrap();
    }
    assert_eq!(
        marlin_diag::register_sink(&SINKS[MAX_SINKS]),
        Err(RegistryFull)
    );

    marlin_diag::error!("full");
    for sink in &SINKS[..MAX_SINKS] {
        assert_eq!(sink.messages_current_thread(), ["full"]);
    }
    assert!(SINKS[MAX_SINKS].messages_current_thread().is_empty());
}
