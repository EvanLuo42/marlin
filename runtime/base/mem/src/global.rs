use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, NonNull};

use marlin_pal::{Protection, vmem};

/// The engine's global allocator. Only final binaries register it:
///
/// ```ignore
/// #[global_allocator]
/// static HEAP: marlin_mem::MarlinHeap = marlin_mem::MarlinHeap::new();
/// ```
///
/// `marlin_rt::game_entry!` does this for the game.
///
/// **Placeholder:** every allocation is its own OS reservation, rounded up to the allocation
/// granularity (64 KiB on Windows). It is correct but wasteful, and is replaced by the
/// mimalloc-style heap.
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

// SAFETY: every allocation is a fresh, committed reservation aligned to the allocation granularity
// (larger alignments are refused), released exactly once in `dealloc`.
unsafe impl GlobalAlloc for MarlinHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.align() > vmem::allocation_granularity() {
            return ptr::null_mut();
        }
        let size = layout.size();
        let Ok(base) = vmem::reserve(size) else {
            return ptr::null_mut();
        };
        // SAFETY: `base..base + size` is the reservation just made.
        if unsafe { vmem::commit(base, size, Protection::ReadWrite) }.is_err() {
            // SAFETY: releases the reservation just made.
            let _ = unsafe { vmem::release(base, size) };
            return ptr::null_mut();
        }
        base.as_ptr()
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // Freshly committed pages are already zero.
        // SAFETY: forwarded from the caller.
        unsafe { self.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: by the GlobalAlloc contract `ptr` came from `alloc` with this layout, so it is a
        // live reservation of `layout.size()` bytes.
        let _ = unsafe { vmem::release(NonNull::new_unchecked(ptr), layout.size()) };
    }
}

#[cfg(test)]
#[allow(clippy::undocumented_unsafe_blocks)]
mod tests {
    use super::*;

    #[test]
    fn alloc_write_dealloc() {
        let heap = MarlinHeap::new();
        let layout = Layout::from_size_align(100, 16).unwrap();
        unsafe {
            let p = heap.alloc_zeroed(layout);
            assert!(!p.is_null());
            assert_eq!(p.addr() % 16, 0);
            assert_eq!(p.add(99).read(), 0);
            p.add(99).write(7);
            heap.dealloc(p, layout);
        }
    }

    #[test]
    fn refuses_alignment_above_granularity() {
        let heap = MarlinHeap::new();
        let align = vmem::allocation_granularity() * 2;
        let layout = Layout::from_size_align(8, align).unwrap();
        assert!(unsafe { heap.alloc(layout) }.is_null());
    }
}
