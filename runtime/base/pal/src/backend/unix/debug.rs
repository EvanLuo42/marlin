use core::ffi::{CStr, c_int, c_void};
use core::mem;

use super::process::copy_utf8;
use super::{Backend, errno};
use crate::backend::imp;
use crate::contract::Debug;
use crate::debug::{Module, Symbol};

impl Debug for Backend {
    fn output(s: &str) {
        let mut bytes = s.as_bytes();
        while !bytes.is_empty() {
            // SAFETY: `bytes` is valid for `bytes.len()` bytes.
            let n = unsafe { libc::write(libc::STDERR_FILENO, bytes.as_ptr().cast(), bytes.len()) };
            if n > 0 {
                bytes = &bytes[n as usize..];
            } else if n < 0 && errno() == libc::EINTR {
                continue;
            } else {
                return;
            }
        }
    }

    fn is_debugger_present() -> bool {
        imp::is_debugger_present()
    }

    fn break_point() {
        // SAFETY: no preconditions.
        unsafe { libc::raise(libc::SIGTRAP) };
    }

    #[inline(never)]
    fn capture_stack(skip: usize, out: &mut [usize]) -> usize {
        // backtrace's first entry is its caller, this function; the public wrapper comes next.
        // Skip both.
        let skip = skip + 2;
        let mut frames = [core::ptr::null_mut::<c_void>(); 256];
        let want = (out.len() + skip).min(frames.len()) as c_int;
        // SAFETY: `frames` holds at least `want` slots.
        let n = unsafe { libc::backtrace(frames.as_mut_ptr(), want) }.max(0) as usize;
        let n = n.saturating_sub(skip).min(out.len());
        for (dst, &src) in out.iter_mut().zip(&frames[skip..skip + n]) {
            *dst = src.addr();
        }
        n
    }

    fn module_of(addr: usize, path: &mut [u8]) -> Option<Module> {
        let info = dladdr(addr)?;
        let path_len = if info.dli_fname.is_null() {
            0
        } else {
            // SAFETY: dladdr fills in a NUL-terminated path owned by the loader.
            copy_utf8(unsafe { CStr::from_ptr(info.dli_fname) }.to_bytes(), path).unwrap_or(0)
        };
        Some(Module {
            base: info.dli_fbase.addr(),
            path_len,
        })
    }

    fn symbolize(addr: usize, name: &mut [u8], _file: &mut [u8]) -> Option<Symbol> {
        let info = dladdr(addr)?;
        if info.dli_sname.is_null() {
            return None;
        }
        // SAFETY: dladdr fills in a NUL-terminated name owned by the loader.
        let name_len =
            copy_utf8(unsafe { CStr::from_ptr(info.dli_sname) }.to_bytes(), name).ok()?;
        Some(Symbol {
            name_len,
            offset: addr.wrapping_sub(info.dli_saddr.addr()),
            file_len: 0,
            line: 0,
        })
    }
}

fn dladdr(addr: usize) -> Option<libc::Dl_info> {
    // SAFETY: plain data that dladdr fills in.
    let mut info: libc::Dl_info = unsafe { mem::zeroed() };
    // SAFETY: any address may be queried; `info` is a valid out pointer.
    (unsafe { libc::dladdr(addr as *const c_void, &mut info) } != 0).then_some(info)
}
