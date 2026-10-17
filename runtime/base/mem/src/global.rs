use core::alloc::{GlobalAlloc, Layout};

/// The engine's general-purpose allocator, a design in the spirit of mimalloc.
///
/// - Memory comes in 4 MiB segments aligned to their size, split into 64 KiB pages (blocks up to
///   8 KiB) or 512 KiB pages (up to 64 KiB). Each page serves one of 48 size classes (at most
///   25% internal waste). Larger requests get a segment of their own.
/// - Each thread allocates from its own heap with no atomics or locks; freeing a block on the
///   thread that owns it is just as cheap. Frees from other threads go onto a lock-free list the
///   owner collects later.
/// - Segments of exited threads are adopted by other threads; empty segments are cached with
///   their memory decommitted. Emptied pages are decommitted after a delay (see
///   [`set_purge_delay`](crate::set_purge_delay)).
///
/// Only final binaries register it:
///
/// ```ignore
/// #[global_allocator]
/// static HEAP: marlin_mem::MarlinHeap = marlin_mem::MarlinHeap::new();
/// ```
///
/// `marlin_rt::game_entry!` does this for the game. It is also a zero-sized
/// [`Allocator`](core::alloc::Allocator) handle onto the same heap, for containers that take one
/// (`Vec::new_in(MarlinHeap::new())`), e.g. to wrap it in [`Tagged`](crate::Tagged). Alignments
/// above 2 MiB are not supported (allocation fails).
///
/// With the `mem-debug` feature, every allocation goes through [`debug`](crate::debug).
#[derive(Clone, Copy, Debug)]
pub struct MarlinHeap {
    _private: (),
}

impl MarlinHeap {
    pub const fn new() -> Self {
        Self { _private: () }
    }
}

impl Default for MarlinHeap {
    fn default() -> Self {
        Self::new()
    }
}

/// The entry points behind both allocator traits: the heap, or with `mem-debug` the debug layer.
/// `free` takes the block's size, which only the debug layer uses.
pub(crate) mod raw {
    #[cfg(feature = "mem-debug")]
    pub(crate) use crate::debug::{free, malloc, malloc_zeroed, realloc};
    #[cfg(not(feature = "mem-debug"))]
    pub(crate) use crate::heap::{malloc, malloc_zeroed, realloc};

    /// # Safety
    /// `p` is null or a live allocation of `size` bytes from this allocator.
    #[cfg(not(feature = "mem-debug"))]
    #[inline(always)]
    pub(crate) unsafe fn free(p: *mut u8, _size: usize) {
        // SAFETY: forwarded from the caller.
        unsafe { crate::heap::free(p) }
    }
}

// SAFETY: `raw::malloc` returns blocks valid for `size` bytes at `align` and unique until freed;
// `free` and `realloc` accept exactly those blocks.
unsafe impl GlobalAlloc for MarlinHeap {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded from the caller.
        unsafe { raw::malloc(layout.size(), layout.align()) }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded from the caller.
        unsafe { raw::malloc_zeroed(layout.size(), layout.align()) }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded from the caller.
        unsafe { raw::free(ptr, layout.size()) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded from the caller.
        unsafe { raw::realloc(ptr, layout.size(), layout.align(), new_size) }
    }
}
