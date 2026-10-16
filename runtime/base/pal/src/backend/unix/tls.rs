use core::ffi::c_void;

use super::{Backend, check_code};
use crate::Result;
use crate::contract::Tls;
use crate::tls::Destructor;

// SAFETY: pthread keys are per thread, start null, and run their destructor at thread exit for
// non-null values.
unsafe impl Tls for Backend {
    type Key = libc::pthread_key_t;

    fn key_create(destructor: Option<Destructor>) -> Result<libc::pthread_key_t> {
        let mut key = 0;
        // SAFETY: `key` is a valid out pointer; the destructor, if any, is valid for the life of
        // the process.
        check_code(unsafe { libc::pthread_key_create(&mut key, destructor) })?;
        Ok(key)
    }

    unsafe fn key_delete(key: libc::pthread_key_t) {
        // SAFETY: the caller guarantees `key` is live.
        unsafe { libc::pthread_key_delete(key) };
    }

    unsafe fn get(key: libc::pthread_key_t) -> *mut c_void {
        // SAFETY: the caller guarantees `key` is live.
        unsafe { libc::pthread_getspecific(key) }
    }

    unsafe fn set(key: libc::pthread_key_t, value: *mut c_void) -> Result<()> {
        // SAFETY: the caller guarantees `key` is live.
        check_code(unsafe { libc::pthread_setspecific(key, value) })
    }
}
