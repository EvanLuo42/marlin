use core::ffi::c_void;

use crate::Result;
use crate::tls::Destructor;

/// # Safety
///
/// Each key must give every thread its own slot, initially null, and must call the destructor at
/// thread exit for slots that are non-null.
pub(crate) unsafe trait Tls {
    type Key: Copy + Send + Sync;

    fn key_create(destructor: Option<Destructor>) -> Result<Self::Key>;
    /// # Safety
    /// `key` is live and is not used again.
    unsafe fn key_delete(key: Self::Key);
    /// # Safety
    /// `key` is live.
    unsafe fn get(key: Self::Key) -> *mut c_void;
    /// # Safety
    /// `key` is live.
    unsafe fn set(key: Self::Key, value: *mut c_void) -> Result<()>;
}
