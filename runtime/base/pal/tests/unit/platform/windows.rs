//! Windows-only behavior: pipe cancellation, command-line splitting, PDB symbols and processor
//! groups.

use marlin_pal::file::{File, OpenOptions, Queue};
use marlin_pal::thread;
use marlin_pal::{CpuSet, ErrorKind, debug};

use crate::cancel::cancel_pending_read;
use crate::stack::outer_marker_function;

/// A named pipe nobody writes to: reads stay pending until cancelled.
fn idle_pipe(name: &str) -> (windows_sys::Win32::Foundation::HANDLE, File) {
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX};
    use windows_sys::Win32::System::Pipes::{CreateNamedPipeW, PIPE_TYPE_BYTE};

    let path = format!(r"\\.\pipe\marlin-pal-{name}-{}", std::process::id());
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    // SAFETY: `wide` is NUL-terminated; null security attributes are allowed.
    let server = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE,
            1,
            4096,
            4096,
            0,
            std::ptr::null(),
        )
    };
    assert_ne!(server, windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE);
    let client = File::open(&path, &OpenOptions::read_write()).unwrap();
    (server, client)
}

#[test]
fn cancel_pending_pipe_read_windows() {
    for fallback in [false, true] {
        let (server, client) = idle_pipe(if fallback { "iocp" } else { "best" });
        let mut queue = if fallback {
            Queue::new_fallback(4).unwrap()
        } else {
            Queue::new(4).unwrap()
        };
        cancel_pending_read(&mut queue, &client);
        drop(queue);
        drop(client);
        // SAFETY: we own the server handle.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(server) };
    }
}

/// Re-runs `args_match_std` in a child with quotes, backslashes and empty arguments on its
/// command line, so the Windows splitter is compared with std's on the hard cases.
#[test]
fn windows_command_line_quoting() {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .raw_arg(r#"process::args_match_std --exact "a b" c\"d "" "e\\\" f" x\y "g""h" "\\""#)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("1 passed"), "{stdout}");
}

/// dbghelp reads the test binary's PDB; POSIX `dladdr` only sees exported symbols, so the names
/// there are not checked.
#[test]
fn symbolizes_with_names_and_lines() {
    let frames = outer_marker_function();
    let mut name = [0u8; 512];
    let mut file = [0u8; 512];
    let found = frames.iter().any(|&addr| {
        let Some(symbol) = debug::symbolize(addr, &mut name, &mut file) else {
            return false;
        };
        let name = std::str::from_utf8(&name[..symbol.name_len]).unwrap();
        let file = std::str::from_utf8(&file[..symbol.file_len]).unwrap();
        name.contains("outer_marker_function") && file.ends_with("stack.rs") && symbol.line > 0
    });
    assert!(found);

    // Concurrent use is serialized internally.
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                let mut name = [0u8; 256];
                let mut file = [0u8; 256];
                for &addr in &frames {
                    let _ = debug::symbolize(addr, &mut name, &mut file);
                }
            });
        }
    });
}

#[test]
fn affinity_spanning_groups_is_rejected() {
    let mut set = CpuSet::new();
    set.insert(0);
    set.insert(64);
    let err = thread::set_current_affinity(&set).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
}
