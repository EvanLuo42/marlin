//! Process environment, memory info, clocks and thread names, checked against std.

use marlin_pal::{ErrorKind, process, sysinfo, thread, time};

fn string(f: impl Fn(&mut [u8]) -> marlin_pal::Result<usize>) -> String {
    let mut buf = vec![0u8; 16];
    loop {
        let len = f(&mut buf).unwrap();
        if len <= buf.len() {
            return String::from_utf8(buf[..len].to_vec()).unwrap();
        }
        buf.resize(len, 0);
    }
}

#[test]
fn args_match_std() {
    let ours: Vec<String> = (0..process::arg_count())
        .map(|i| {
            let mut buf = vec![0u8; 4096];
            let len = process::arg(i, &mut buf).unwrap().unwrap();
            String::from_utf8(buf[..len].to_vec()).unwrap()
        })
        .collect();
    let std: Vec<String> = std::env::args().collect();
    assert_eq!(ours, std);
    assert_eq!(process::arg(ours.len(), &mut [0; 8]).unwrap(), None);
}

#[test]
fn arg_reports_full_length_when_truncated() {
    let mut small = [0u8; 2];
    let len = process::arg(0, &mut small).unwrap().unwrap();
    assert_eq!(len, std::env::args().next().unwrap().len());
}

#[test]
fn exe_dir_and_id_match_std() {
    let exe = string(process::current_exe);
    assert_eq!(std::path::Path::new(&exe), std::env::current_exe().unwrap());
    let dir = string(process::current_dir);
    assert_eq!(std::path::Path::new(&dir), std::env::current_dir().unwrap());
    assert_eq!(process::id(), std::process::id());
}

#[test]
fn env_var() {
    let name = "MARLIN_PAL_TEST_VAR_\u{e9}";
    // SAFETY: no other test in this binary touches this variable or reads the environment
    // through libc while it changes.
    unsafe { std::env::set_var(name, "value \u{1F41F}") };
    assert_eq!(
        string(|out| Ok(process::env_var(name, out)?.unwrap())),
        "value \u{1F41F}"
    );

    // SAFETY: as above.
    unsafe { std::env::set_var(name, "") };
    assert_eq!(process::env_var(name, &mut [0; 4]).unwrap(), Some(0));

    // SAFETY: as above.
    unsafe { std::env::remove_var(name) };
    assert_eq!(process::env_var(name, &mut [0; 4]).unwrap(), None);

    for bad in ["", "A=B", "A\0B"] {
        let error = process::env_var(bad, &mut [0; 4]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
    }
}

#[test]
fn set_current_dir_round_trips() {
    // The working directory is process-wide; this is the only test that changes it, and it
    // restores it before asserting.
    let original = std::env::current_dir().unwrap();
    let target = std::env::temp_dir();
    process::set_current_dir(target.to_str().unwrap()).unwrap();
    let now = std::env::current_dir().unwrap();
    std::env::set_current_dir(&original).unwrap();
    assert_eq!(now.canonicalize().unwrap(), target.canonicalize().unwrap());
    let error = process::set_current_dir("").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
}

#[test]
fn memory_is_plausible() {
    let info = sysinfo::memory().unwrap();
    assert!(info.total_physical > 64 << 20, "{info:?}");
    assert!(info.available_physical <= info.total_physical, "{info:?}");
    assert!(info.process_resident > 0, "{info:?}");
    assert!(
        info.process_peak_resident >= info.process_resident,
        "{info:?}"
    );
    assert!(info.process_private > 0, "{info:?}");
    if let (Some(limit), Some(committed)) = (info.commit_limit, info.committed) {
        assert!(committed <= limit, "{info:?}");
    }

    // Touching 64 MiB raises the resident set by most of it.
    let before = sysinfo::memory().unwrap().process_resident;
    let block = vec![1u8; 64 << 20];
    std::hint::black_box(&block);
    let after = sysinfo::memory().unwrap().process_resident;
    assert!(after >= before + (48 << 20), "{before} -> {after}");
}

#[test]
fn system_time_matches_std() {
    let std = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let (secs, nanos) = time::system_time();
    assert!(nanos < 1_000_000_000);
    let ours = std::time::Duration::new(secs as u64, nanos);
    let diff = ours.abs_diff(std);
    assert!(diff < std::time::Duration::from_secs(1), "{diff:?}");
}

#[test]
fn local_offset_is_plausible() {
    let (now, _) = time::system_time();
    let offset = time::local_offset(now).unwrap();
    // UTC-12 to UTC+14, in whole quarter hours.
    assert!((-12 * 3600..=14 * 3600).contains(&offset), "{offset}");
    assert_eq!(offset % 900, 0, "{offset}");
    // Half a year apart, offsets differ by at most the DST shift.
    let other = time::local_offset(now - 182 * 86400).unwrap();
    assert!((offset - other).abs() <= 3600, "{offset} {other}");
}

#[test]
fn cycles_advance() {
    let a = time::cycles();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let b = time::cycles();
    assert!(b > a);
}

#[test]
fn thread_name_round_trips() {
    std::thread::spawn(|| {
        // Unnamed threads inherit the process name on Linux.
        if cfg!(windows) {
            assert_eq!(string(thread::current_name), "");
        }
        thread::set_current_name("pal-name");
        assert_eq!(string(thread::current_name), "pal-name");
        // Linux truncates to 15 bytes.
        thread::set_current_name("a-much-longer-thread-name");
        let name = string(thread::current_name);
        assert!("a-much-longer-thread-name".starts_with(&name), "{name}");
        assert!(name.len() >= 15, "{name}");
    })
    .join()
    .unwrap();
}

/// Re-runs `args_match_std` in a child with quotes, backslashes and empty arguments on its
/// command line, so the Windows splitter is compared with std's on the hard cases.
#[cfg(windows)]
#[test]
fn windows_command_line_quoting() {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .raw_arg(r#"args_match_std --exact "a b" c\"d "" "e\\\" f" x\y "g""h" "\\""#)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("1 passed"), "{stdout}");
}
