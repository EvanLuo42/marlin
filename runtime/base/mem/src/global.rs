use core::alloc::{GlobalAlloc, Layout};

use crate::heap;

/// The engine's general-purpose allocator, a design in the spirit of mimalloc.
///
/// - Memory comes in 4 MiB segments aligned to their size, split into 64 KiB pages (blocks up to
///   8 KiB) or 512 KiB pages (up to 64 KiB). Each page serves one of 48 size classes (at most
///   25% internal waste). Larger requests get a segment of their own.
/// - Each thread allocates from its own heap with no atomics or locks; freeing a block on the
///   thread that owns it is just as cheap. Frees from other threads go onto a lock-free list the
///   owner collects later.
/// - Segments of exited threads are adopted by other threads; empty segments are cached with
///   their memory decommitted.
///
/// Only final binaries register it:
///
/// ```ignore
/// #[global_allocator]
/// static HEAP: marlin_mem::MarlinHeap = marlin_mem::MarlinHeap::new();
/// ```
///
/// `marlin_rt::game_entry!` does this for the game. Alignments above 2 MiB are not supported
/// (allocation returns null).
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

// SAFETY: `heap::malloc` returns blocks valid for `size` bytes at `align` and unique until freed;
// `free` and `realloc` accept exactly those blocks.
unsafe impl GlobalAlloc for MarlinHeap {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded from the caller.
        unsafe { heap::malloc(layout.size(), layout.align()) }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded from the caller.
        unsafe { heap::malloc_zeroed(layout.size(), layout.align()) }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        // SAFETY: forwarded from the caller.
        unsafe { heap::free(ptr) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded from the caller.
        unsafe { heap::realloc(ptr, layout.size(), layout.align(), new_size) }
    }
}
