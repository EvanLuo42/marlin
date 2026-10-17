//! The `crash_dump` extension: dumps of the running process, and a crash handler run in a child
//! process that faults on purpose (quick tier, docs/design/0014-testing.md §5). Needs the
//! `crash_dump` extension; no hardware.
//! Run alone with `cargo test -p marlin-pal --test isolated_crash_dump`.
#![cfg(marlin_ext = "crash_dump")]

use marlin_pal::ext::crash_dump::{CrashDump, CrashInfo};
use marlin_test_support::{TempDir, TempFile, isolated};

/// Where the child writes its dump.
const DUMP_DIR_ENV: &str = "MARLIN_TEST_DUMP_DIR";

#[test]
fn writes_dump_of_running_process() {
    let path = TempFile::new("pal.dmp");
    CrashDump::get()
        .unwrap()
        .write(path.to_str().unwrap(), false)
        .unwrap();
    let len = std::fs::metadata(&path).unwrap().len();
    std::fs::remove_file(&path).unwrap();
    // Header, thread list, module list and stacks: well over a few KiB.
    assert!(len > 8 << 10, "{len}");
}

fn on_crash(info: &CrashInfo) {
    let path = format!("{}/crash.dmp", std::env::var(DUMP_DIR_ENV).unwrap());
    info.write_dump(&path, false).unwrap();
    // Exit with a recognizable code instead of letting WER handle the crash.
    let code = if info.code == 0xC000_0005 && info.access == Some((16, true)) {
        42
    } else {
        43
    };
    std::process::exit(code);
}

#[test]
#[ignore = "child of handler_sees_access_violation"]
fn crash_child() {
    if !isolated::is_child() {
        return;
    }
    CrashDump::get().unwrap().set_handler(Some(on_crash));
    // SAFETY: deliberately faults; the handler ends the process.
    unsafe { std::ptr::without_provenance_mut::<u8>(16).write_volatile(1) };
    unreachable!();
}

#[test]
fn handler_sees_access_violation() {
    let dir = TempDir::new("pal-crash");
    let output = isolated::run("crash_child", &[(DUMP_DIR_ENV, dir.as_ref())]);
    assert_eq!(output.status.code(), Some(42));
    let path = dir.join("crash.dmp");
    let len = std::fs::metadata(&path).unwrap().len();
    std::fs::remove_file(&path).unwrap();
    assert!(len > 8 << 10, "{len}");
}
