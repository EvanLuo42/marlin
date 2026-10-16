//! Dynamic thread-local storage keys with exit destructors.
//!
//! For hot data use `#[thread_local]` statics directly; keys exist for what statics cannot do,
//! chiefly running a destructor when a thread exits (e.g. returning a thread's heap to the
//! allocator).
//!
//! Windows implements keys with fiber-local storage (FLS), so values follow Windows fibers
//! (`CreateFiber`) rather than threads. Marlin's job system switches fibers itself, which keeps FLS
//! thread-local.

use core::ffi::c_void;

use crate::Result;
use crate::backend::imp::Backend;
use crate::contract::Tls;

/// Called at thread exit with the thread's value, for threads whose value is non-null.
///
/// Not guaranteed to run for the main thread or at process exit. Deleting a key runs the
/// destructors of every thread's value on Windows but not on POSIX, so clear values first if that
/// matters.
pub type Destructor = unsafe extern "C" fn(value: *mut c_void);

/// A thread-local slot, initially null on every thread.
#[derive(Clone, Copy)]
pub struct Key(<Backend as Tls>::Key);

impl Key {
    pub fn new(destructor: Option<Destructor>) -> Result<Self> {
        Backend::key_create(destructor).map(Self)
    }

    /// # Safety
    ///
    /// The key must not be used afterwards, by any thread.
    #[inline]
    pub unsafe fn delete(self) {
        // SAFETY: forwarded from the caller.
        unsafe { Backend::key_delete(self.0) }
    }

    /// # Safety
    ///
    /// The key must not have been deleted.
    #[inline]
    pub unsafe fn get(self) -> *mut c_void {
        // SAFETY: forwarded from the caller.
        unsafe { Backend::get(self.0) }
    }

    /// # Safety
    ///
    /// The key must not have been deleted.
    #[inline]
    pub unsafe fn set(self, value: *mut c_void) -> Result<()> {
        // SAFETY: forwarded from the caller.
        unsafe { Backend::set(self.0, value) }
    }
}
