//! The process API, the startup sequence and the hosted panic report.

use marlin_rt::process;

#[test]
fn args_match_std() {
    let ours: Vec<&str> = process::all_args().to_vec();
    let std: Vec<String> = std::env::args().collect();
    assert_eq!(ours, std);
    assert_eq!(process::args().collect::<Vec<_>>(), &ours[1..]);
    // Cached: the same slice every time.
    assert_eq!(process::all_args().as_ptr(), process::all_args().as_ptr());
}

#[test]
fn environment_and_paths() {
    assert_eq!(process::id(), std::process::id());
    assert_eq!(
        std::path::Path::new(&process::current_exe().unwrap()),
        std::env::current_exe().unwrap()
    );
    assert_eq!(
        std::path::Path::new(&process::current_dir().unwrap()),
        std::env::current_dir().unwrap()
    );
    // SAFETY: no other test in this binary touches this variable.
    unsafe { std::env::set_var("MARLIN_RT_TEST_VAR", "\u{1F41F} fish") };
    assert_eq!(
        process::env_var("MARLIN_RT_TEST_VAR").as_deref(),
        Some("\u{1F41F} fish")
    );
    assert_eq!(process::env_var("MARLIN_RT_TEST_VAR_UNSET"), None);
}

/// The startup sequence installs the crash handler with dumps in `MARLIN_CRASH_DIR`, and a
/// hosted panic report logs the panic with a backtrace and writes a dump there. Runs in a child
/// so the process-wide handler and the deliberate panic stay out of this test binary.
#[test]
fn hosted_startup_and_panic_report() {
    let dir = std::env::temp_dir().join(format!("marlin-rt-crash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["panic_child", "--exact", "--ignored", "--nocapture"])
        .env("MARLIN_CRASH_DIR", &dir)
        .env("MARLIN_RT_PANIC_CHILD", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("panicked at"), "{stderr}");
    assert!(stderr.contains("deliberate test panic"), "{stderr}");
    #[cfg(windows)]
    assert!(stderr.contains("panic_child"), "{stderr}");
    let dumps: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    if cfg!(marlin_ext = "crash_dump") {
        assert_eq!(dumps.len(), 1, "{stderr}");
        assert!(stderr.contains("crash dump written to"), "{stderr}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[ignore]
fn panic_child() {
    if std::env::var_os("MARLIN_RT_PANIC_CHILD").is_none() {
        return;
    }
    marlin_rt::hosted::init().unwrap();
    std::panic::set_hook(Box::new(|info| {
        let message = info.payload_as_str().unwrap_or("?");
        marlin_rt::hosted::report_panic(&message, info.location());
    }));
    panic!("deliberate test panic");
}
