use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A global allocator over `A` that counts live and peak bytes and aborts the process once live
/// bytes would pass `cap`, so a runaway fuzz, robustness or stress test fails at once instead of
/// eating the machine's memory.
///
/// ```ignore
/// #[global_allocator]
/// static HEAP: MemCap<std::alloc::System> = MemCap::new(std::alloc::System, 1 << 30);
/// ```
pub struct MemCap<A> {
    inner: A,
    cap: usize,
    live: AtomicUsize,
    peak: AtomicUsize,
}

impl<A> MemCap<A> {
    pub const fn new(inner: A, cap: usize) -> Self {
        Self {
            inner,
            cap,
            live: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }

    /// Bytes allocated and not yet freed.
    pub fn live(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    /// The most bytes live at once so far.
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }

    fn grow(&self, size: usize) {
        let live = self.live.fetch_add(size, Ordering::Relaxed) + size;
        if live > self.cap {
            // No allocating here: the formatting machinery would recurse into us.
            let message = b"MemCap: the test passed its memory cap; aborting\n";
            use std::io::Write;
            let _ = std::io::stderr().write_all(message);
            std::process::abort();
        }
        self.peak.fetch_max(live, Ordering::Relaxed);
    }

    fn shrink(&self, size: usize) {
        self.live.fetch_sub(size, Ordering::Relaxed);
    }
}

// SAFETY: every call forwards to `inner` with the same arguments; the counters only observe.
unsafe impl<A: GlobalAlloc> GlobalAlloc for MemCap<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.grow(layout.size());
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { self.inner.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        self.grow(layout.size());
        // SAFETY: as for `alloc`.
        unsafe { self.inner.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.shrink(layout.size());
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { self.inner.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            self.grow(new_size - layout.size());
        } else {
            self.shrink(layout.size() - new_size);
        }
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { self.inner.realloc(ptr, layout, new_size) }
    }
}
