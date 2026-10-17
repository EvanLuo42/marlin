use core::mem;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::WriteFile;
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE, WriteConsoleW,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    DebugBreak, IMAGEHLP_LINEW64, IsDebuggerPresent, OutputDebugStringW, RtlCaptureStackBackTrace,
    SYMBOL_INFOW, SYMOPT_DEFERRED_LOADS, SYMOPT_LOAD_LINES, SYMOPT_UNDNAME, SymFromAddrW,
    SymGetLineFromAddrW64, SymInitializeW, SymSetOptions,
};

use windows_sys::Win32::System::Environment::GetEnvironmentVariableW;
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::{Backend, for_each_wide_chunk, utf16_to_utf8};
use crate::contract::{Debug, Futex};
use crate::debug::{Module, Symbol};

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

    #[inline(never)]
    fn capture_stack(skip: usize, out: &mut [usize]) -> usize {
        let count = out.len().min(u16::MAX as usize) as u32;
        // Skip this function and the public wrapper too; the wrapper is inline(never).
        let skip = (skip + 2).min(u32::MAX as usize) as u32;
        // SAFETY: `out` holds `count` pointer-sized slots.
        let n = unsafe {
            RtlCaptureStackBackTrace(skip, count, out.as_mut_ptr().cast(), ptr::null_mut())
        };
        n as usize
    }

    fn module_of(addr: usize, path: &mut [u8]) -> Option<Module> {
        let mut module = ptr::null_mut();
        // SAFETY: with FROM_ADDRESS the "name" is an address inside the module; UNCHANGED_REFCOUNT
        // means nothing needs releasing.
        let found = unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                addr as *const u16,
                &mut module,
            )
        } != 0;
        if !found {
            return None;
        }
        let mut buf = [0u16; 1024];
        // SAFETY: `module` is loaded; `buf` holds its length in units.
        let n = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) };
        let path_len = utf16_to_utf8(&buf[..n as usize], path).unwrap_or(0);
        Some(Module {
            base: module.addr(),
            path_len,
        })
    }

    fn symbolize(addr: usize, name: &mut [u8], file: &mut [u8]) -> Option<Symbol> {
        // dbghelp is single-threaded: every call goes through this lock.
        DBGHELP.lock();
        let result = symbolize_locked(addr, name, file);
        DBGHELP.unlock();
        result
    }
}

/// A futex lock for dbghelp, which is not thread-safe.
struct Lock(AtomicU32);

impl Lock {
    fn lock(&self) {
        while self
            .0
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            <Backend as Futex>::wait(&self.0, 1, None);
        }
    }

    fn unlock(&self) {
        self.0.store(0, Ordering::Release);
        <Backend as Futex>::wake_one(&self.0);
    }
}

static DBGHELP: Lock = Lock(AtomicU32::new(0));
/// 0 = not initialized, 1 = ready, 2 = failed. Only read and written under `DBGHELP`.
static DBGHELP_STATE: AtomicU32 = AtomicU32::new(0);

/// Writes `<exe dir>;.;<_NT_SYMBOL_PATH>` with a NUL into `out`; returns the units before the NUL,
/// or 0 if the executable path is unavailable.
fn symbol_search_path(out: &mut [u16; 4096]) -> usize {
    // SAFETY: a null module means the executable; `out` holds its length in units.
    let n = unsafe { GetModuleFileNameW(ptr::null_mut(), out.as_mut_ptr(), out.len() as u32) };
    let n = n as usize;
    if n == 0 || n >= out.len() {
        return 0;
    }
    let Some(slash) = out[..n].iter().rposition(|&c| c == u16::from(b'\\')) else {
        return 0;
    };
    let mut len = slash;
    for unit in ";.".encode_utf16() {
        out[len] = unit;
        len += 1;
    }
    let name = "_NT_SYMBOL_PATH\0".encode_utf16();
    let mut wide_name = [0u16; 16];
    for (dst, unit) in wide_name.iter_mut().zip(name) {
        *dst = unit;
    }
    out[len] = u16::from(b';');
    let room = (out.len() - len - 2) as u32;
    // SAFETY: the name is NUL-terminated; the output holds `room` units after the separator.
    let env =
        unsafe { GetEnvironmentVariableW(wide_name.as_ptr(), out[len + 1..].as_mut_ptr(), room) };
    if env > 0 && env < room {
        len += 1 + env as usize;
    }
    out[len] = 0;
    len
}

/// Longest symbol name read, in UTF-16 units.
const MAX_NAME: usize = 1024;

fn symbolize_locked(addr: usize, name: &mut [u8], file: &mut [u8]) -> Option<Symbol> {
    // SAFETY: a pseudo-handle for the current process.
    let process = unsafe { GetCurrentProcess() };
    if DBGHELP_STATE.load(Ordering::Relaxed) == 0 {
        // The default search path (working directory and _NT_SYMBOL_PATH) misses the
        // executable's own directory, where its PDB usually is.
        let mut search = [0u16; 4096];
        let search_len = symbol_search_path(&mut search);
        let search = if search_len > 0 {
            search.as_ptr()
        } else {
            ptr::null()
        };
        // SAFETY: called once, under the lock; `search` is null or NUL-terminated. Invading the
        // process registers every module already loaded; deferred loads read symbols lazily.
        let ok = unsafe {
            SymSetOptions(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_LOAD_LINES);
            SymInitializeW(process, search, 1) != 0
        };
        DBGHELP_STATE.store(if ok { 1 } else { 2 }, Ordering::Relaxed);
    }
    if DBGHELP_STATE.load(Ordering::Relaxed) != 1 {
        return None;
    }

    // SYMBOL_INFOW ends in a variable-length name.
    #[repr(C, align(8))]
    struct Buf {
        info: SYMBOL_INFOW,
        name: [u16; MAX_NAME],
    }
    // SAFETY: plain data; all-zero is valid.
    let mut buf: Buf = unsafe { mem::zeroed() };
    buf.info.SizeOfStruct = size_of::<SYMBOL_INFOW>() as u32;
    buf.info.MaxNameLen = MAX_NAME as u32;
    let mut displacement = 0u64;
    // SAFETY: `buf` holds a SYMBOL_INFOW followed by room for `MaxNameLen` units.
    if unsafe { SymFromAddrW(process, addr as u64, &mut displacement, &mut buf.info) } == 0 {
        return None;
    }
    let len = (buf.info.NameLen as usize).min(MAX_NAME);
    // SAFETY: the name starts at `info.Name` and runs into `buf.name`, `len` units in all.
    let units =
        unsafe { core::slice::from_raw_parts((&raw const buf.info.Name).cast::<u16>(), len) };
    let name_len = utf16_to_utf8(units, name).unwrap_or(0);

    let mut line = IMAGEHLP_LINEW64 {
        SizeOfStruct: size_of::<IMAGEHLP_LINEW64>() as u32,
        ..Default::default()
    };
    let mut line_displacement = 0u32;
    // SAFETY: `line` is a valid IMAGEHLP_LINEW64 with its size set.
    let has_line =
        unsafe { SymGetLineFromAddrW64(process, addr as u64, &mut line_displacement, &mut line) }
            != 0;
    let (file_len, line_number) = if has_line && !line.FileName.is_null() {
        // SAFETY: dbghelp returns a NUL-terminated name it owns until the next call, which the
        // lock rules out while we copy.
        let units = unsafe {
            let mut n = 0;
            while *line.FileName.add(n) != 0 {
                n += 1;
            }
            core::slice::from_raw_parts(line.FileName, n)
        };
        (utf16_to_utf8(units, file).unwrap_or(0), line.LineNumber)
    } else {
        (0, 0)
    };
    Some(Symbol {
        name_len,
        offset: displacement as usize,
        file_len,
        line: line_number,
    })
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
