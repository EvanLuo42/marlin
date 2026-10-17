//! The crash handler, run in a child process that faults on purpose (quick tier,
//! docs/design/0014-testing.md §5). Its own binary: `install` is once per process. Needs the
//! `crash_dump` extension. Run alone with `cargo test -p marlin-diag --test isolated_crash_dump`.
#![cfg(marlin_ext = "crash_dump")]

use marlin_diag::ext::crash_dump;
use marlin_test_support::{TempDir, isolated};

/// Where the child writes its dump.
const DUMP_DIR_ENV: &str = "MARLIN_TEST_DUMP_DIR";

fn dumps_in(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "dmp"))
        .collect()
}

#[test]
#[ignore = "child of unhandled_crash_is_reported"]
fn crash_child() {
    if !isolated::is_child() {
        return;
    }
    let dir = std::env::var(DUMP_DIR_ENV).unwrap();
    // No Windows Error Reporting dialog for the deliberate crash.
    #[cfg(marlin_platform = "windows")]
    // SAFETY: no preconditions.
    unsafe {
        use windows_sys::Win32::System::Diagnostics::Debug::{
            SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
        };
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
    }
    crash_dump::install(&dir).unwrap();
    // SAFETY: deliberately faults; the handler reports it and the process ends.
    unsafe { std::ptr::without_provenance_mut::<u32>(0x40).write_volatile(1) };
    unreachable!();
}

#[test]
fn unhandled_crash_is_reported() {
    let dir = TempDir::new("diag-crash");
    let output = isolated::run("crash_child", &[(DUMP_DIR_ENV, dir.as_ref())]);
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
}

#[test]
fn write_now_after_install() {
    // `install` is once per process, so this test owns it in this binary.
    assert!(crash_dump::write_now().is_none());
    let dir = TempDir::new("diag-now");
    let dir_str = dir.to_str().unwrap();
    crash_dump::install(dir_str).unwrap();
    assert!(crash_dump::is_installed());
    assert_eq!(
        crash_dump::install(dir_str),
        Err(crash_dump::InstallError::AlreadyInstalled)
    );
    let path = crash_dump::write_now().unwrap();
    let name = path.as_str().rsplit('/').next().unwrap();
    assert!(
        name.starts_with("crash-") && name.ends_with(".dmp"),
        "{name}"
    );
    assert!(std::fs::metadata(path.as_str()).unwrap().len() > 8 << 10);
}
