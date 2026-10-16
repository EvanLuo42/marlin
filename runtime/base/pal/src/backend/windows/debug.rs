use core::ptr;

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::WriteFile;
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE, WriteConsoleW,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    DebugBreak, IsDebuggerPresent, OutputDebugStringW,
};

use super::{Backend, for_each_wide_chunk};
use crate::contract::Debug;

impl Debug for Backend {
    fn output(s: &str) {
        write_stderr(s);
        if Self::is_debugger_present() {
            // SAFETY: each chunk is NUL-terminated.
            for_each_wide_chunk(s, |chunk| unsafe { OutputDebugStringW(chunk.as_ptr()) });
        }
    }

    fn is_debugger_present() -> bool {
        // SAFETY: no preconditions.
        unsafe { IsDebuggerPresent() != 0 }
    }

    fn break_point() {
        // SAFETY: no preconditions.
        unsafe { DebugBreak() }
    }
}

fn write_stderr(s: &str) {
    // SAFETY: no preconditions.
    let handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    // GUI-subsystem processes have no stderr.
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return;
    }

    let mut mode = 0;
    // SAFETY: `mode` is a valid out pointer.
    if unsafe { GetConsoleMode(handle, &mut mode) } != 0 {
        // A console: write UTF-16 so text survives whatever code page the console uses.
        for_each_wide_chunk(s, |chunk| {
            let text = &chunk[..chunk.len() - 1];
            let mut written = 0;
            // SAFETY: `text` is valid for `text.len()` code units; `written` is a valid out pointer.
            unsafe {
                WriteConsoleW(
                    handle,
                    text.as_ptr(),
                    text.len() as u32,
                    &mut written,
                    ptr::null(),
                )
            };
        });
        return;
    }

    // A file or pipe: raw UTF-8.
    let mut bytes = s.as_bytes();
    while !bytes.is_empty() {
        let len = bytes.len().min(u32::MAX as usize) as u32;
        let mut written = 0;
        // SAFETY: `bytes` is valid for `len` bytes; synchronous write, so no OVERLAPPED.
        let ok = unsafe { WriteFile(handle, bytes.as_ptr(), len, &mut written, ptr::null_mut()) };
        if ok == 0 || written == 0 {
            return;
        }
        bytes = &bytes[written as usize..];
    }
}
