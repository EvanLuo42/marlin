//! `LoadLibraryExW` / `GetProcAddress` / `FreeLibrary`.

use core::ffi::{CStr, c_void};
use core::ptr::{self, NonNull};

use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW,
};

use super::error::last_error;
use super::{Backend, with_wide};
use crate::Result;
use crate::contract::ext::Dylib;

pub(crate) struct Library(HMODULE);

// SAFETY: module handles may be used and freed from any thread.
unsafe impl Send for Library {}
// SAFETY: GetProcAddress is thread-safe.
unsafe impl Sync for Library {}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: we own one reference to the module.
        unsafe { FreeLibrary(self.0) };
    }
}

impl Dylib for Backend {
    type Library = Library;

    fn dylib_open(path: &str) -> Result<Library> {
        // With a path, look for the library's own dependencies next to it.
        let flags = if path.contains(['/', '\\']) {
            LOAD_WITH_ALTERED_SEARCH_PATH
        } else {
            0
        };
        // LoadLibrary wants `\` separators.
        let mut native = [0u8; crate::file::MAX_PATH_LEN];
        for (dst, &b) in native.iter_mut().zip(path.as_bytes()) {
            *dst = if b == b'/' { b'\\' } else { b };
        }
        // Replacing ASCII `/` with `\` keeps the bytes UTF-8.
        let native = core::str::from_utf8(&native[..path.len()]).unwrap_or(path);
        with_wide(native, |path| {
            // SAFETY: `path` is NUL-terminated; no file handle is passed.
            let module = unsafe { LoadLibraryExW(path, ptr::null_mut(), flags) };
            if module.is_null() {
                Err(last_error())
            } else {
                Ok(Library(module))
            }
        })
    }

    fn dylib_symbol(library: &Library, name: &CStr) -> Option<NonNull<c_void>> {
        // SAFETY: the module is loaded; `name` is NUL-terminated.
        let proc = unsafe { GetProcAddress(library.0, name.as_ptr().cast()) }?;
        NonNull::new(proc as *mut c_void)
    }
}
