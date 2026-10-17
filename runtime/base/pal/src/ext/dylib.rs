//! Loading shared libraries at run time (desktop only): editor plugins, optional vendor SDKs.

use core::ffi::CStr;

use crate::backend::imp::Backend;
use crate::contract::ext::Dylib as Contract;
use crate::file::MAX_PATH_LEN;
use crate::{Error, ErrorKind, Result};

/// Proof that this platform loads shared libraries. Obtain one with [`Dylib::get`].
#[derive(Clone, Copy, Debug)]
pub struct Dylib(());

impl Dylib {
    #[inline]
    pub fn get() -> Option<Self> {
        Some(Self(()))
    }

    /// Loads the library at `path`: a path (dependencies are then also looked up next to it on
    /// Windows) or a bare file name searched the platform's way. `NotFound` if there is no such
    /// file, `InvalidData` (POSIX) or the OS error (Windows) if it cannot be loaded.
    pub fn open(self, path: &str) -> Result<Library> {
        if path.is_empty() || path.len() > MAX_PATH_LEN || path.as_bytes().contains(&0) {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        Backend::dylib_open(path).map(Library)
    }
}

/// A loaded shared library. Dropping it unloads the library, invalidating every symbol taken
/// from it.
pub struct Library(<Backend as Contract>::Library);

impl Library {
    /// The address of the exported symbol `name`, as `T` (a function pointer or raw pointer
    /// type), or `None` if the library does not export it.
    ///
    /// # Safety
    ///
    /// `T` must match the symbol's real type, and nothing may use the result after the library
    /// is dropped.
    ///
    /// # Panics
    /// If `T` is not pointer-sized.
    pub unsafe fn symbol<T: Copy>(&self, name: &CStr) -> Option<T> {
        assert_eq!(
            size_of::<T>(),
            size_of::<*const ()>(),
            "symbol type must be a pointer"
        );
        let addr = Backend::dylib_symbol(&self.0, name)?;
        // SAFETY: `T` is pointer-sized, and the caller guarantees it is the symbol's type.
        Some(unsafe { core::mem::transmute_copy::<*mut core::ffi::c_void, T>(&addr.as_ptr()) })
    }
}
