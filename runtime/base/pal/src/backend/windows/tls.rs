//! TLS keys on fiber-local storage: unlike `TlsAlloc`, FLS runs a callback at thread exit.

use core::ffi::c_void;

use windows_sys::Win32::System::Threading::{
    FLS_OUT_OF_INDEXES, FlsAlloc, FlsFree, FlsGetValue, FlsSetValue,
};

use super::Backend;
use super::error::{check, last_error};
use crate::Result;
use crate::contract::Tls;
use crate::tls::Destructor;

// The destructor cast below relies on "system" being the C calling convention.
#[cfg(not(target_pointer_width = "64"))]
compile_error!("the Windows backend supports 64-bit targets only");

// SAFETY: FLS slots are per thread (Marlin never uses Windows fibers), start null, and FLS calls
// the callback at thread exit for non-null slots.
unsafe impl Tls for Backend {
    type Key = u32;

    fn key_create(destructor: Option<Destructor>) -> Result<u32> {
        // SAFETY: on 64-bit Windows `extern "system"` is `extern "C"`, and `*mut`/`*const`
        // pointers are ABI-identical, so this only changes the type, not the call.
        let callback = destructor.map(|d| unsafe {
            core::mem::transmute::<Destructor, unsafe extern "system" fn(*const c_void)>(d)
        });
        // SAFETY: the callback, if any, is a valid function for the life of the process.
        let index = unsafe { FlsAlloc(callback) };
        if index == FLS_OUT_OF_INDEXES {
            Err(last_error())
        } else {
            Ok(index)
        }
    }

    unsafe fn key_delete(key: u32) {
        // SAFETY: the caller guarantees `key` is live.
        unsafe { FlsFree(key) };
    }

    unsafe fn get(key: u32) -> *mut c_void {
        // SAFETY: the caller guarantees `key` is live.
        unsafe { FlsGetValue(key) }
    }

    unsafe fn set(key: u32, value: *mut c_void) -> Result<()> {
        // SAFETY: the caller guarantees `key` is live.
        check(unsafe { FlsSetValue(key, value) })
    }
}
