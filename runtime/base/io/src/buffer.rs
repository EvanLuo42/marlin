use alloc::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use core::fmt;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;
use core::slice;

use crate::File;

/// A zeroed, heap-allocated byte buffer with a chosen alignment, owned by an [`IoQueue`] while an
/// operation on it is in flight.
///
/// Owning the buffer (rather than borrowing it) is what makes the queue safe: if a queue is
/// leaked with operations in flight, their buffers leak with it instead of being freed under the
/// OS.
///
/// [`IoQueue`]: crate::IoQueue
pub struct IoBuffer {
    ptr: NonNull<u8>,
    len: usize,
    align: usize,
}

// SAFETY: the buffer owns its bytes, like `Box<[u8]>`.
unsafe impl Send for IoBuffer {}
// SAFETY: as above; shared access is read-only.
unsafe impl Sync for IoBuffer {}

impl IoBuffer {
    /// `len` zero bytes.
    pub fn new(len: usize) -> Self {
        Self::aligned(len, 1)
    }

    /// `len` zero bytes at an address that is a multiple of `align`.
    ///
    /// # Panics
    /// If `align` is not a power of two, or the size overflows.
    pub fn aligned(len: usize, align: usize) -> Self {
        let layout = Layout::from_size_align(len, align).expect("invalid IoBuffer layout");
        let ptr = if len == 0 {
            // Dangling but aligned, like an empty Vec.
            NonNull::new(core::ptr::without_provenance_mut(align)).unwrap()
        } else {
            // SAFETY: the layout has a non-zero size.
            let ptr = unsafe { alloc_zeroed(layout) };
            NonNull::new(ptr).unwrap_or_else(|| handle_alloc_error(layout))
        };
        Self { ptr, len, align }
    }

    /// A buffer `file` can use directly: aligned to [`File::alignment`], with `len` rounded up to
    /// a multiple of it.
    pub fn for_file(file: &File, len: usize) -> Self {
        let align = file.alignment();
        Self::aligned(len.next_multiple_of(align), align)
    }

    #[inline]
    pub fn align(&self) -> usize {
        self.align
    }

    #[inline]
    pub(crate) fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr.as_ptr()
    }
}

impl Drop for IoBuffer {
    fn drop(&mut self) {
        if self.len != 0 {
            // SAFETY: allocated in `aligned` with this layout.
            unsafe {
                dealloc(
                    self.ptr.as_ptr(),
                    Layout::from_size_align_unchecked(self.len, self.align),
                )
            };
        }
    }
}

impl Deref for IoBuffer {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        // SAFETY: `ptr` is valid for `len` initialized bytes (zeroed at allocation).
        unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl DerefMut for IoBuffer {
    #[inline]
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above, and `&mut self` makes the access unique.
        unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl fmt::Debug for IoBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IoBuffer")
            .field("len", &self.len)
            .field("align", &self.align)
            .finish_non_exhaustive()
    }
}
