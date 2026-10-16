//! Large (huge) pages: fewer TLB misses for big, long-lived allocations.

use core::ptr::NonNull;

use crate::backend::imp::Backend;
use crate::contract::ext::LargePages as Contract;
use crate::{Error, ErrorKind, Protection, Result};

/// Proof that this process can allocate large pages. Obtain one with [`LargePages::get`].
#[derive(Clone, Copy, Debug)]
pub struct LargePages {
    page_size: usize,
}

impl LargePages {
    /// Returns `None` when the OS or the account does not allow large pages (on Windows the
    /// account needs the "Lock pages in memory" privilege).
    #[inline]
    pub fn get() -> Option<Self> {
        Backend::large_page_size().map(|page_size| Self { page_size })
    }

    #[inline]
    pub fn page_size(self) -> usize {
        self.page_size
    }

    /// Reserves and commits `size` bytes backed by large pages, aligned to [`page_size`].
    ///
    /// `size` must be a non-zero multiple of [`page_size`]. Large pages are never paged out and
    /// cannot be decommitted; free the allocation as a whole with [`crate::vmem::release`].
    ///
    /// [`page_size`]: Self::page_size
    #[inline]
    pub fn alloc(self, size: usize, prot: Protection) -> Result<NonNull<u8>> {
        if size == 0 || !size.is_multiple_of(self.page_size) {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        Backend::alloc_large(size, prot)
    }
}
