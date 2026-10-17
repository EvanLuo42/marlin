use core::ffi::{CStr, c_void};
use core::ptr::NonNull;

use crate::Result;

pub(crate) trait Dylib {
    /// A loaded library; dropping it unloads it.
    type Library: Send + Sync;

    /// `path` is non-empty, at most `MAX_PATH_LEN` bytes and has no NUL (checked by the caller).
    fn dylib_open(path: &str) -> Result<Self::Library>;
    fn dylib_symbol(library: &Self::Library, name: &CStr) -> Option<NonNull<c_void>>;
}
