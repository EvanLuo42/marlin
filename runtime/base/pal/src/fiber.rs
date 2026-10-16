//! Fibers: user-mode execution contexts switched explicitly, the basis of the job system.
//!
//! A [`Context`] is the saved stack pointer of a suspended fiber. [`make_context`] prepares one
//! that starts running `entry(arg)` on a [`Stack`]; [`switch`] saves the current context and
//! resumes another. Any thread can resume any suspended context, so a fiber may migrate between
//! threads: never cache a thread-local's address across a switch.
//!
//! The switch is hand-written per calling convention (see `arch/`) and costs a few nanoseconds.
//! Windows fibers (`CreateFiber`) are not used: Marlin's TLS keys are FLS, which would follow
//! Windows fibers instead of threads.

use core::ffi::c_void;
use core::ptr::NonNull;

use crate::backend::imp::Backend;
use crate::contract::Fiber;
use crate::{Protection, Result, vmem};

/// Runs on a fresh context. It must never return; finish by switching away for good.
pub type Entry = unsafe extern "C" fn(arg: *mut c_void) -> !;

/// The saved stack pointer of a suspended fiber.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Context(pub(crate) *mut u8);

impl Context {
    /// A placeholder for [`switch`] to save into.
    pub const NULL: Self = Self(core::ptr::null_mut());
}

// SAFETY: a context is an address; resuming it is what needs care, and that is unsafe.
unsafe impl Send for Context {}

/// A fiber stack: committed memory with an inaccessible guard page below it, so an overflow
/// faults instead of corrupting memory.
pub struct Stack {
    base: NonNull<u8>,
    size: usize,
}

// SAFETY: plain memory owned by this value.
unsafe impl Send for Stack {}
// SAFETY: `&Stack` only exposes addresses.
unsafe impl Sync for Stack {}

impl Stack {
    /// A stack with at least `size` usable bytes (rounded up to whole pages).
    pub fn new(size: usize) -> Result<Self> {
        let page = vmem::page_size();
        let usable = size.max(page).next_multiple_of(page);
        let total = usable + page;
        let base = vmem::reserve(total)?;
        // SAFETY: commits all of the reservation just made but its lowest page.
        let committed = unsafe { vmem::commit(base.add(page), usable, Protection::ReadWrite) };
        if let Err(error) = committed {
            // SAFETY: releases the reservation just made.
            let _ = unsafe { vmem::release(base, total) };
            return Err(error);
        }
        Ok(Self { base, size: total })
    }

    /// Lowest address of the reservation, the start of the guard page.
    pub fn base(&self) -> *mut u8 {
        self.base.as_ptr()
    }

    /// Lowest usable address, just above the guard page.
    pub fn limit(&self) -> *mut u8 {
        // SAFETY: within the reservation.
        unsafe { self.base.as_ptr().add(vmem::page_size()) }
    }

    /// One past the highest usable address; stacks grow down from here.
    pub fn top(&self) -> *mut u8 {
        // SAFETY: one past the end of the reservation.
        unsafe { self.base.as_ptr().add(self.size) }
    }

    /// Usable bytes.
    pub fn size(&self) -> usize {
        self.size - vmem::page_size()
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        // SAFETY: we own the reservation; whoever made contexts on it has stopped using them.
        let _ = unsafe { vmem::release(self.base, self.size) };
    }
}

/// Prepares a context that, when first switched to, runs `entry(arg)` on `stack`.
///
/// # Safety
///
/// `stack` must outlive every use of the context, and must not hold another live context.
#[inline]
pub unsafe fn make_context(stack: &Stack, entry: Entry, arg: *mut c_void) -> Context {
    // SAFETY: forwarded from the caller.
    unsafe { Backend::make_context(stack, entry, arg) }
}

/// Saves the current execution context into `*save` and resumes `to`. Returns when something
/// switches back to the saved context, possibly on another thread.
///
/// # Safety
///
/// `save` must be valid for writes. `to` must come from [`make_context`] or a previous `switch`,
/// and be suspended: not running, and not about to be resumed by another thread.
#[inline]
pub unsafe fn switch(save: *mut Context, to: Context) {
    // SAFETY: forwarded from the caller.
    unsafe { Backend::switch(save, to) }
}
