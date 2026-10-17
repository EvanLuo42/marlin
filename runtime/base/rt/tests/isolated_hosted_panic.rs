//! The hosted startup sequence and panic report, run in a child process that panics on purpose
//! (quick tier, docs/design/0014-testing.md §5). No hardware needed; the dump is checked where
//! the `crash_dump` extension is. Run alone with
//! `cargo test -p marlin-rt --test isolated_hosted_panic`.

use marlin_test_support::{TempDir, isolated};

/// The startup sequence installs the crash handler with dumps in `MARLIN_CRASH_DIR`, and a
/// hosted panic report logs the panic with a backtrace and writes a dump there. Runs in a child
/// so the process-wide handler and the deliberate panic stay out of this test binary.
#[test]
fn hosted_startup_and_panic_report() {
    let dir = TempDir::new("rt-crash");
    let output = isolated::run("panic_child", &[("MARLIN_CRASH_DIR", dir.as_ref())]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("panicked at"), "{stderr}");
    assert!(stderr.contains("deliberate test panic"), "{stderr}");
    #[cfg(marlin_platform = "windows")]
    assert!(stderr.contains("panic_child"), "{stderr}");
    let dumps: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    if cfg!(marlin_ext = "crash_dump") {
        assert_eq!(dumps.len(), 1, "{stderr}");
        assert!(stderr.contains("crash dump written to"), "{stderr}");
    }
}

#[test]
#[ignore = "child of hosted_startup_and_panic_report"]
fn panic_child() {
    if !isolated::is_child() {
        return;
    }
    marlin_rt::hosted::init().unwrap();
    std::panic::set_hook(Box::new(|info| {
        let message = info.payload_as_str().unwrap_or("?");
        marlin_rt::hosted::report_panic(&message, info.location());
    }));
    panic!("deliberate test panic");
}
