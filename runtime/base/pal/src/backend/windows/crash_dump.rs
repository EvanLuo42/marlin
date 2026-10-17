//! Minidumps with `MiniDumpWriteDump` and the unhandled-exception filter.

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

use windows_sys::Win32::Foundation::{
    CloseHandle, EXCEPTION_ACCESS_VIOLATION, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL};
use windows_sys::Win32::System::Diagnostics::Debug::{
    EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION, MINIDUMP_TYPE, MiniDumpWithFullMemory,
    MiniDumpWithHandleData, MiniDumpWithIndirectlyReferencedMemory, MiniDumpWithProcessThreadData,
    MiniDumpWithThreadInfo, MiniDumpWithUnloadedModules, MiniDumpWriteDump,
    SetUnhandledExceptionFilter,
};
use windows_sys::Win32::System::Threading::{
    CreateThread, GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, INFINITE,
    WaitForSingleObject,
};

use super::error::last_error;
use super::{Backend, with_wide_path};
use crate::contract::ext::CrashDump;
use crate::ext::crash_dump::{CrashHandler, CrashInfo};
use crate::{Error, ErrorKind, Result};

/// `EXCEPTION_CONTINUE_SEARCH`: let the OS go on (to WER and termination).
const CONTINUE_SEARCH: i32 = 0;

/// The installed handler, as a raw function pointer; null when none.
static HANDLER: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());

/// What the helper thread needs; lives on the requesting thread's stack until it is joined.
struct Job {
    file: HANDLE,
    exception: *mut EXCEPTION_POINTERS,
    thread_id: u32,
    kind: MINIDUMP_TYPE,
    ok: bool,
}

unsafe extern "system" fn dump_thread(param: *mut c_void) -> u32 {
    // SAFETY: `param` is the `Job` the requester waits on, alive until we return.
    unsafe {
        let job = &mut *param.cast::<Job>();
        let info = MINIDUMP_EXCEPTION_INFORMATION {
            ThreadId: job.thread_id,
            ExceptionPointers: job.exception,
            ClientPointers: 0,
        };
        let info_ptr = if job.exception.is_null() {
            ptr::null()
        } else {
            &raw const info
        };
        job.ok = MiniDumpWriteDump(
            GetCurrentProcess(),
            GetCurrentProcessId(),
            job.file,
            job.kind,
            info_ptr,
            ptr::null(),
            ptr::null(),
        ) != 0;
    }
    0
}

unsafe extern "system" fn filter(pointers: *const EXCEPTION_POINTERS) -> i32 {
    let handler = HANDLER.load(Ordering::Acquire);
    if handler.is_null() || pointers.is_null() {
        return CONTINUE_SEARCH;
    }
    // SAFETY: the pointer was stored from a `CrashHandler`; the OS passes valid records.
    unsafe {
        let handler = core::mem::transmute::<*mut (), CrashHandler>(handler);
        let record = &*(*pointers).ExceptionRecord;
        let access = (record.ExceptionCode == EXCEPTION_ACCESS_VIOLATION
            && record.NumberParameters >= 2)
            .then(|| {
                (
                    record.ExceptionInformation[1],
                    record.ExceptionInformation[0] == 1,
                )
            });
        handler(&CrashInfo {
            code: record.ExceptionCode as u32,
            address: record.ExceptionAddress.addr(),
            access,
            raw: pointers.cast_mut().cast(),
        });
    }
    CONTINUE_SEARCH
}

impl CrashDump for Backend {
    unsafe fn write_dump(path: &str, exception: *mut c_void, full_memory: bool) -> Result<()> {
        let file = with_wide_path(path, "", |path| {
            // SAFETY: `path` is NUL-terminated; null attributes and template are allowed.
            let file = unsafe {
                CreateFileW(
                    path,
                    GENERIC_WRITE,
                    0,
                    ptr::null(),
                    CREATE_ALWAYS,
                    FILE_ATTRIBUTE_NORMAL,
                    ptr::null_mut(),
                )
            };
            if file == INVALID_HANDLE_VALUE {
                Err(last_error())
            } else {
                Ok(file)
            }
        })?;
        let kind = if full_memory {
            MiniDumpWithFullMemory
                | MiniDumpWithHandleData
                | MiniDumpWithThreadInfo
                | MiniDumpWithUnloadedModules
                | MiniDumpWithProcessThreadData
        } else {
            MiniDumpWithIndirectlyReferencedMemory
                | MiniDumpWithThreadInfo
                | MiniDumpWithUnloadedModules
                | MiniDumpWithProcessThreadData
        };
        let mut job = Job {
            file,
            exception: exception.cast(),
            // SAFETY: no preconditions.
            thread_id: unsafe { GetCurrentThreadId() },
            kind,
            ok: false,
        };
        // A helper thread writes the dump, so this thread's stack is captured where it waits
        // instead of deep inside dbghelp.
        // SAFETY: `job` outlives the thread, which is joined below.
        let thread = unsafe {
            CreateThread(
                ptr::null(),
                0,
                Some(dump_thread),
                (&raw mut job).cast(),
                0,
                ptr::null_mut(),
            )
        };
        let result = if thread.is_null() {
            Err(last_error())
        } else {
            // SAFETY: we own the thread handle.
            unsafe {
                WaitForSingleObject(thread, INFINITE);
                CloseHandle(thread);
            }
            if job.ok {
                Ok(())
            } else {
                Err(Error::from_kind(ErrorKind::Other))
            }
        };
        // SAFETY: we own the file handle.
        unsafe { CloseHandle(file) };
        result
    }

    fn set_crash_handler(handler: Option<CrashHandler>) {
        let raw = handler.map_or(ptr::null_mut(), |f| f as *mut ());
        HANDLER.store(raw, Ordering::Release);
        // SAFETY: `filter` matches LPTOP_LEVEL_EXCEPTION_FILTER; None restores the default.
        unsafe { SetUnhandledExceptionFilter(handler.map(|_| filter as _)) };
    }
}
