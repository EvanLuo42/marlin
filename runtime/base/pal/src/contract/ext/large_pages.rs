use core::ptr::NonNull;

use crate::contract::VMem;
use crate::{Protection, Result};

/// # Safety
///
/// Allocations must be aligned to the large page size and releasable with [`VMem::release`].
pub(crate) unsafe trait LargePages: VMem {
    /// The large page size if this process can allocate large pages, `None` otherwise.
    ///
    /// May acquire privileges on the first call; must be cheap afterwards.
    fn large_page_size() -> Option<usize>;

    /// Reserves and commits `size` bytes backed by large pages. `size` is a non-zero multiple of
    /// the large page size (checked by the caller).
    fn alloc_large(size: usize, prot: Protection) -> Result<NonNull<u8>>;
}
