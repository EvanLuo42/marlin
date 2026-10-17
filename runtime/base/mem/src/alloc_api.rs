//! [`Allocator`] for [`MarlinHeap`], so containers can name the heap: `Vec::new_in(MarlinHeap::new())`.
//!
//! Growing and shrinking go through the heap's `realloc`, which stays in place within a block's
//! size class (and for huge blocks, within their reservation). Zero-sized requests get a dangling
//! pointer and allocate nothing.

use core::alloc::{AllocError, Allocator, Layout};
use core::ptr::{self, NonNull};

use crate::global::{MarlinHeap, raw};

#[inline]
fn block(p: *mut u8, size: usize) -> Result<NonNull<[u8]>, AllocError> {
    match NonNull::new(p) {
        Some(p) => Ok(NonNull::slice_from_raw_parts(p, size)),
        None => Err(AllocError),
    }
}

#[inline]
fn empty(layout: Layout) -> NonNull<[u8]> {
    NonNull::slice_from_raw_parts(layout.dangling_ptr(), 0)
}

impl MarlinHeap {
    /// Grows or shrinks a block, in place when the heap can.
    ///
    /// # Safety
    /// Same contract as [`Allocator::grow`] or [`Allocator::shrink`], whichever applies.
    #[inline]
    unsafe fn resize(
        &self,
        ptr: NonNull<u8>,
        old: Layout,
        new: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        if old.size() == 0 {
            return self.allocate(new);
        }
        if new.size() == 0 {
            // SAFETY: forwarded from the caller.
            unsafe { self.deallocate(ptr, old) };
            return Ok(empty(new));
        }
        if ptr.addr().get().is_multiple_of(new.align()) {
            // SAFETY: a live block of `old.size()` bytes; the heap moves it to `new.align()` if it
            // has to.
            let p = unsafe { raw::realloc(ptr.as_ptr(), old.size(), new.align(), new.size()) };
            return block(p, new.size());
        }
        let to = self.allocate(new)?;
        // SAFETY: both blocks are live and distinct; the copy fits in both.
        unsafe {
            ptr::copy_nonoverlapping(ptr.as_ptr(), to.as_ptr().cast(), old.size().min(new.size()));
            self.deallocate(ptr, old);
        }
        Ok(to)
    }
}

// SAFETY: blocks come from the same heap as `GlobalAlloc`, valid for their layout and unique
// until freed; zero-sized blocks are dangling and never passed to the heap. `resize` keeps the
// block on failure.
unsafe impl Allocator for MarlinHeap {
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() == 0 {
            return Ok(empty(layout));
        }
        // SAFETY: a non-zero size.
        block(
            unsafe { raw::malloc(layout.size(), layout.align()) },
            layout.size(),
        )
    }

    #[inline]
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() == 0 {
            return Ok(empty(layout));
        }
        // SAFETY: a non-zero size.
        block(
            unsafe { raw::malloc_zeroed(layout.size(), layout.align()) },
            layout.size(),
        )
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        if layout.size() != 0 {
            // SAFETY: a block of `layout.size()` bytes from this heap (the caller).
            unsafe { raw::free(ptr.as_ptr(), layout.size()) }
        }
    }

    #[inline]
    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old: Layout,
        new: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // SAFETY: forwarded from the caller.
        unsafe { self.resize(ptr, old, new) }
    }

    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old: Layout,
        new: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // SAFETY: forwarded from the caller; the bytes past `old.size()` are part of the new block.
        unsafe {
            let to = self.resize(ptr, old, new)?;
            let tail = to.as_ptr().cast::<u8>().add(old.size());
            ptr::write_bytes(tail, 0, new.size() - old.size());
            Ok(to)
        }
    }

    #[inline]
    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old: Layout,
        new: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // SAFETY: forwarded from the caller.
        unsafe { self.resize(ptr, old, new) }
    }
}
