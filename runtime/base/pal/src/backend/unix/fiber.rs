use core::ffi::c_void;

use super::Backend;
use crate::contract::Fiber;
use crate::fiber::{Context, Entry, Stack};

#[cfg(target_arch = "aarch64")]
use crate::arch::aarch64::aapcs as abi;
#[cfg(target_arch = "x86_64")]
use crate::arch::x86_64::sysv as abi;

// SAFETY: the System V / AAPCS64 variants preserve the ABI's callee-saved state.
unsafe impl Fiber for Backend {
    #[inline]
    unsafe fn make_context(stack: &Stack, entry: Entry, arg: *mut c_void) -> Context {
        // SAFETY: forwarded from the caller.
        unsafe { abi::make_context(stack, entry, arg) }
    }

    #[inline]
    unsafe fn switch(save: *mut Context, to: Context) {
        // SAFETY: forwarded from the caller.
        unsafe { abi::switch(save, to) }
    }
}
