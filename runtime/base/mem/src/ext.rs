//! Capability extensions of the PAL that belong to memory; each module exists only where the
//! platform manifest lists the capability (see `marlin_pal::ext`).

/// Large pages: fewer TLB misses for big, long-lived allocations.
#[cfg(marlin_ext = "large_pages")]
pub mod large_pages {
    pub use marlin_pal::ext::large_pages::LargePages;

    use crate::Arena;

    /// An [`Arena`] backed by large pages, committed in full when created (large pages cannot be
    /// committed piecemeal) and never trimmed. For level-lifetime data that is walked often.
    ///
    /// `capacity` is rounded up to the large page size. Fails when the OS has no large pages to
    /// give (on Linux they must be reserved with `vm.nr_hugepages`; on Windows the account needs
    /// "Lock pages in memory").
    pub fn arena(lp: LargePages, capacity: usize) -> marlin_pal::Result<Arena> {
        let size = capacity.max(1).next_multiple_of(lp.page_size());
        let base = lp.alloc(size, marlin_pal::Protection::ReadWrite)?;
        // SAFETY: a fresh committed read-write allocation, released with `vmem::release`.
        Ok(unsafe { Arena::from_committed(base, size) })
    }
}

/// Memory mapped at two addresses: mirrored ring buffers, and discarding contents in place.
#[cfg(marlin_ext = "vmem_alias")]
pub mod vmem_alias {
    use core::ptr::NonNull;

    use marlin_pal::ext::vmem_alias::Mirror;
    pub use marlin_pal::ext::vmem_alias::VmemAlias;

    /// A ring buffer whose memory is mapped twice in a row, so any `len <= capacity` bytes from
    /// any offset are contiguous: `as_ptr().add(offset % capacity)` is valid for `capacity` bytes.
    /// Producers write and consumers read whole records without splitting them at the end.
    pub struct MirroredRing {
        mirror: Mirror,
    }

    impl MirroredRing {
        /// A zeroed ring of at least `min_capacity` bytes, rounded up to the allocation
        /// granularity (64 KiB on Windows, the page size elsewhere).
        pub fn new(alias: VmemAlias, min_capacity: usize) -> marlin_pal::Result<Self> {
            let size = min_capacity.max(1).next_multiple_of(alias.granularity());
            Ok(Self {
                mirror: alias.mirror(size)?,
            })
        }

        #[inline]
        pub fn capacity(&self) -> usize {
            self.mirror.size()
        }

        /// The start of the ring; `capacity()` more bytes past any offset below `capacity()` are
        /// mapped.
        #[inline]
        pub fn as_ptr(&self) -> *mut u8 {
            self.mirror.as_ptr()
        }

        /// `len` bytes from `offset` (taken modulo the capacity), contiguous across the end.
        ///
        /// # Panics
        /// If `len > capacity()`.
        ///
        /// # Safety
        /// No other reference to those bytes, through either view, may be alive while the slice
        /// is.
        #[allow(clippy::mut_from_ref)]
        pub unsafe fn slice_mut(&self, offset: usize, len: usize) -> &mut [u8] {
            assert!(len <= self.capacity(), "slice longer than the ring");
            // SAFETY: `offset % capacity + len <= 2 * capacity`, inside the two views; the caller
            // rules out aliasing.
            unsafe {
                core::slice::from_raw_parts_mut(self.as_ptr().add(offset % self.capacity()), len)
            }
        }

        /// Lets the OS drop the contents of the whole ring (both views) without unmapping it, for
        /// a ring that will sit empty for a while.
        ///
        /// # Safety
        /// Nothing may rely on the ring's contents afterwards.
        pub unsafe fn discard(&self, alias: VmemAlias) -> marlin_pal::Result<()> {
            // SAFETY: the first view is committed read-write; the caller gives up the contents.
            unsafe { alias.reset(NonNull::new_unchecked(self.as_ptr()), self.capacity()) }
        }
    }
}
