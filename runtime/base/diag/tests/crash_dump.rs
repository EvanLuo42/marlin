//! The crash handler, run in a child process that faults on purpose.
#![cfg(marlin_ext = "crash_dump")]

use marlin_diag::ext::crash_dump;

const CHILD_ENV: &str = "MARLIN_DIAG_CRASH_DIR";

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("marlin-diag-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn dumps_in(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "dmp"))
        .collect()
}

/// Runs only as the child of `unhandled_crash_is_reported`.
#[test]
#[ignore]
fn crash_child() {
    let Some(dir) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    // No Windows Error Reporting dialog for the deliberate crash.
    #[cfg(windows)]
    // SAFETY: no preconditions.
    unsafe {
        use windows_sys::Win32::System::Diagnostics::Debug::{
            SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
        };
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
    }
    crash_dump::install(dir.to_str().unwrap()).unwrap();
    // SAFETY: deliberately faults; the handler reports it and the process ends.
    unsafe { std::ptr::without_provenance_mut::<u32>(0x40).write_volatile(1) };
    unreachable!();
}

#[test]
fn unhandled_crash_is_reported() {
    let dir = temp_dir("crash");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["crash_child", "--exact", "--ignored", "--nocapture"])
        .env(CHILD_ENV, &dir)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("unhandled exception 0xc0000005"),
        "{stderr}"
    );
    assert!(stderr.contains("writing 0x40"), "{stderr}");
    // The backtrace reaches the faulting test function.
    assert!(stderr.contains("crash_child"), "{stderr}");
    let dumps = dumps_in(&dir);
    assert_eq!(dumps.len(), 1, "{stderr}");
    assert!(
        stderr.contains(dumps[0].file_name().unwrap().to_str().unwrap()),
        "{stderr}"
    );
    assert!(std::fs::metadata(&dumps[0]).unwrap().len() > 8 << 10);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn write_now_after_install() {
    // `install` is once per process, so this test owns it in this binary.
    assert!(crash_dump::write_now().is_none());
    let dir = temp_dir("now");
    crash_dump::install(dir.to_str().unwrap()).unwrap();
    assert!(crash_dump::is_installed());
    assert_eq!(
        crash_dump::install(dir.to_str().unwrap()),
        Err(crash_dump::InstallError::AlreadyInstalled)
    );
    let path = crash_dump::write_now().unwrap();
    let name = path.as_str().rsplit('/').next().unwrap();
    assert!(
        name.starts_with("crash-") && name.ends_with(".dmp"),
        "{name}"
    );
    assert!(std::fs::metadata(path.as_str()).unwrap().len() > 8 << 10);
    std::fs::remove_dir_all(&dir).unwrap();
}
