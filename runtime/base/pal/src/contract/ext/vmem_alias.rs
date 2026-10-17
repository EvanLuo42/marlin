use core::ptr::NonNull;

use crate::Result;
use crate::contract::VMem;

/// # Safety
///
/// `mirror_alloc` must return `2 * size` bytes of read-write memory where both halves map the
/// same pages, released only by `mirror_free`.
pub(crate) unsafe trait VmemAlias: VMem {
    /// `size` is a non-zero multiple of the allocation granularity (checked by the caller).
    fn mirror_alloc(size: usize) -> Result<NonNull<u8>>;
    /// # Safety
    /// `base`/`size` are exactly what `mirror_alloc` returned and was given.
    unsafe fn mirror_free(base: NonNull<u8>, size: usize) -> Result<()>;
    /// Tells the OS it may discard the contents of the committed range without decommitting it.
    ///
    /// # Safety
    /// The range is page aligned and committed read-write.
    unsafe fn reset(ptr: NonNull<u8>, size: usize) -> Result<()>;
}
