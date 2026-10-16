//! Fills the global registry, so this binary holds a single test.

mod common;

use common::Capture;
use marlin_diag::{MAX_SINKS, RegistryFull};

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
