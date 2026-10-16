use core::ffi::c_void;

use crate::fiber::{Context, Entry, Stack};

/// Backends usually delegate to the variant in `crate::arch` matching their calling convention.
///
/// # Safety
///
/// `switch` must preserve every callee-saved register and the floating-point control state of
/// the platform ABI, plus any per-thread state tied to the stack (e.g. the Windows TEB bounds).
pub(crate) unsafe trait Fiber {
    /// # Safety
    /// `stack` outlives every use of the context.
    unsafe fn make_context(stack: &Stack, entry: Entry, arg: *mut c_void) -> Context;
    /// # Safety
    /// `save` is writable; `to` is a suspended context.
    unsafe fn switch(save: *mut Context, to: Context);
}
