//! Shipping builds record nothing but keep the API.
#![cfg(marlin_build = "shipping")]

use marlin_profile::{ENABLED, frame_mark, profile_scope, set_thread_name, with_history};

#[test]
fn shipping_records_nothing() {
    const { assert!(!ENABLED) };
    set_thread_name("main");
    {
        profile_scope!("zone");
    }
    frame_mark();
    with_history(|h| {
        assert_eq!(h.frames().len(), 0);
        let mut json = Vec::new();
        h.write_chrome_trace(&mut json).unwrap();
        let trace: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert!(trace["traceEvents"].is_array());
    });
}
