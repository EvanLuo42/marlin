//! Mapping the same physical pages at several addresses, and discarding contents in place.
//!
//! [`Mirror`] maps one block of memory twice, back to back, so a ring buffer can hand out
//! contiguous slices that wrap around its end. [`VmemAlias::reset`] (`MEM_RESET`, `MADV_FREE`,
//! `MADV_FREE_REUSABLE`) lets the OS drop committed pages whose contents no longer matter without
//! the cost of decommitting and recommitting them.
//!
//! | Platform | Mirror |
//! |---|---|
//! | Windows | a pagefile-backed section mapped into two halves of a placeholder (Windows 10 1803+) |
//! | Linux | a `memfd` mapped twice |
//! | macOS | an unlinked POSIX shared memory object mapped twice |

use core::ptr::NonNull;

use crate::backend::imp::Backend;
use crate::contract::VMem;
use crate::contract::ext::VmemAlias as Contract;
use crate::{Error, ErrorKind, Result};

/// Proof that this platform can alias memory. Obtain one with [`VmemAlias::get`].
#[derive(Clone, Copy, Debug)]
pub struct VmemAlias(());

impl VmemAlias {
    #[inline]
    pub fn get() -> Option<Self> {
        Some(Self(()))
    }

    /// Size granularity of [`mirror`](Self::mirror): the allocation granularity (64 KiB on
    /// Windows, the page size elsewhere).
    #[inline]
    pub fn granularity(self) -> usize {
        Backend::allocation_granularity()
    }

    /// Allocates `size` bytes of zeroed, read-write memory mapped twice in a row: byte `i` and
    /// byte `i + size` are the same. `size` must be a non-zero multiple of
    /// [`granularity`](Self::granularity).
    pub fn mirror(self, size: usize) -> Result<Mirror> {
        if size == 0 || !size.is_multiple_of(self.granularity()) || size > isize::MAX as usize / 2 {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        Backend::mirror_alloc(size).map(|base| Mirror { base, size })
    }

    /// Lets the OS discard the contents of committed pages, which stay committed and accessible.
    /// Until written again they read as their old contents or as zero, unpredictably.
    ///
    /// # Safety
    ///
    /// The range must be page aligned and committed read-write, and nothing may rely on its
    /// contents.
    pub unsafe fn reset(self, ptr: NonNull<u8>, size: usize) -> Result<()> {
        if size == 0 {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        // SAFETY: forwarded from the caller.
        unsafe { Backend::reset(ptr, size) }
    }
}

/// Memory mapped twice in a row; see [`VmemAlias::mirror`]. Dropping it unmaps both views.
pub struct Mirror {
    base: NonNull<u8>,
    size: usize,
}

// SAFETY: plain memory owned by the mirror; access goes through raw pointers.
unsafe impl Send for Mirror {}
// SAFETY: as above.
unsafe impl Sync for Mirror {}

impl Mirror {
    /// The start of the first view; the second starts at `as_ptr() + size()`.
    #[inline]
    pub fn as_ptr(&self) -> *mut u8 {
        self.base.as_ptr()
    }

    /// Size of one view.
    #[inline]
    pub fn size(&self) -> usize {
        self.size
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        // SAFETY: exactly what `mirror_alloc` returned; nothing uses it any more.
        let _ = unsafe { Backend::mirror_free(self.base, self.size) };
    }
}
