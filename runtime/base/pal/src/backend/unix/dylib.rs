//! `dlopen` / `dlsym` / `dlclose`.

use core::ffi::{CStr, c_void};
use core::ptr::NonNull;

use super::{Backend, with_c_path};
use crate::contract::ext::Dylib;
use crate::{Error, ErrorKind, Result};

pub(crate) struct Library(NonNull<c_void>);

// SAFETY: dl handles may be used and closed from any thread.
unsafe impl Send for Library {}
// SAFETY: dlsym is thread-safe.
unsafe impl Sync for Library {}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: we own one reference to the library.
        unsafe { libc::dlclose(self.0.as_ptr()) };
    }
}

impl Dylib for Backend {
    type Library = Library;

    fn dylib_open(path: &str) -> Result<Library> {
        with_c_path(path, |c_path| {
            // SAFETY: `c_path` is NUL-terminated. RTLD_LOCAL keeps the library's symbols out of
            // the global namespace.
            let handle = unsafe { libc::dlopen(c_path, libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if let Some(handle) = NonNull::new(handle) {
                return Ok(Library(handle));
            }
            // dlerror only gives text; tell a missing file from one that does not load.
            // SAFETY: `c_path` is NUL-terminated.
            let exists = path.contains('/') && unsafe { libc::access(c_path, libc::F_OK) } == 0;
            Err(Error::from_kind(if exists {
                ErrorKind::InvalidData
            } else {
                ErrorKind::NotFound
            }))
        })
    }

    fn dylib_symbol(library: &Library, name: &CStr) -> Option<NonNull<c_void>> {
        // SAFETY: the library is loaded; `name` is NUL-terminated.
        NonNull::new(unsafe { libc::dlsym(library.0.as_ptr(), name.as_ptr()) })
    }
}
