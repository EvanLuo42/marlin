//! Linear arenas: bump allocation from one reserved range, freed all at once.
//!
//! [`Arena`] reserves address space up front and commits it in [`COMMIT_GRANULE`] steps as
//! allocations reach it, so a large reservation costs nothing until used. Allocation is a single
//! atomic bump, safe from any number of threads at once: the parallel jobs of one frame share its
//! arena. Committed memory is kept across resets, so a frame arena stops touching the OS once it
//! has seen its largest frame ([`Arena::trim`] gives memory back).
//!
//! - [`Arena`]: the linear allocator; `&Arena` is a [`core::alloc::Allocator`], so
//!   `Vec::new_in(&arena)` and friends grow in place while they are the last allocation.
//! - [`FrameArenas`]: a ring of arenas for a pipeline of overlapping frames.
//! - [`Arena::scope`]: stack-style use, everything allocated inside is released after.
//!
//! Values in an arena are never dropped: arenas suit plain data and arena-backed containers, not
//! types that own other memory.

mod frame;

pub use frame::FrameArenas;

use core::alloc::{AllocError, Allocator, Layout};
use core::fmt;
use core::mem::MaybeUninit;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicUsize, Ordering};

use marlin_pal::{Protection, vmem};

use crate::lock::Lock;

/// Memory is committed in steps of this many bytes (a multiple of the page size on every
/// platform Marlin targets).
pub const COMMIT_GRANULE: usize = 64 << 10;

/// Bytes written over released memory in debug builds, so reads of stale arena data stand out.
#[cfg(debug_assertions)]
const POISON: u8 = 0xcd;

/// A linear allocator over a reserved address range. See the [module docs](self).
pub struct Arena {
    base: NonNull<u8>,
    reserved: usize,
    /// Bytes handed out, from `base`.
    used: AtomicUsize,
    /// Bytes committed, from `base`; a multiple of the granule (or `reserved`).
    committed: AtomicUsize,
    /// Most bytes used since the last trim: what [`Arena::trim`] keeps by default.
    peak: AtomicUsize,
    commit: Lock,
}

// SAFETY: the arena owns its reservation; allocation is atomic, and commits happen under `commit`.
unsafe impl Send for Arena {}
// SAFETY: as above.
unsafe impl Sync for Arena {}

/// A position in an arena to [`rewind`](Arena::rewind) to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Checkpoint(usize);

impl Arena {
    /// Reserves `capacity` bytes of address space (rounded up to the OS's granularity). Nothing is
    /// committed until used.
    pub fn new(capacity: usize) -> marlin_pal::Result<Self> {
        let reserved = capacity
            .max(1)
            .next_multiple_of(vmem::allocation_granularity());
        let base = vmem::reserve(reserved)?;
        Ok(Self {
            base,
            reserved,
            used: AtomicUsize::new(0),
            committed: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            commit: Lock::new(),
        })
    }

    /// Bytes of address space reserved.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.reserved
    }

    /// Bytes handed out since the last reset, alignment padding included.
    #[inline]
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    /// Bytes of physical memory committed.
    #[inline]
    pub fn committed(&self) -> usize {
        self.committed.load(Ordering::Relaxed)
    }

    /// `layout`'s worth of memory, or `None` once the reservation (or the OS) runs out.
    #[inline]
    pub fn try_alloc_layout(&self, layout: Layout) -> Option<NonNull<u8>> {
        let base = self.base.as_ptr() as usize;
        let mut used = self.used.load(Ordering::Relaxed);
        let (start, end) = loop {
            // Aligned as an address, not as an offset: the base is only granule-aligned.
            let start = (base + used).checked_next_multiple_of(layout.align())? - base;
            let end = start.checked_add(layout.size())?;
            if end > self.reserved {
                return None;
            }
            match self
                .used
                .compare_exchange_weak(used, end, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break (start, end),
                Err(now) => used = now,
            }
        };
        if end > self.committed.load(Ordering::Acquire) {
            self.commit_to(end)?;
        }
        // SAFETY: `start <= reserved`, inside the reservation.
        Some(unsafe { self.base.add(start) })
    }

    /// Commits memory through byte `end`.
    #[cold]
    fn commit_to(&self, end: usize) -> Option<()> {
        self.commit.lock();
        let committed = self.committed.load(Ordering::Relaxed);
        let mut ok = true;
        if end > committed {
            let target = end.next_multiple_of(COMMIT_GRANULE).min(self.reserved);
            // SAFETY: `committed..target` is granule-aligned (or ends the reservation) and inside
            // it.
            ok = unsafe {
                vmem::commit(
                    self.base.add(committed),
                    target - committed,
                    Protection::ReadWrite,
                )
            }
            .is_ok();
            if ok {
                self.committed.store(target, Ordering::Release);
            }
        }
        self.commit.unlock();
        ok.then_some(())
    }

    /// `layout`'s worth of memory. Panics when the arena is exhausted.
    #[inline]
    pub fn alloc_layout(&self, layout: Layout) -> NonNull<u8> {
        match self.try_alloc_layout(layout) {
            Some(p) => p,
            None => exhausted(self, layout),
        }
    }

    /// Moves `value` into the arena. It is never dropped.
    #[inline]
    #[expect(clippy::mut_from_ref, reason = "each allocation is disjoint")]
    pub fn alloc<T>(&self, value: T) -> &mut T {
        let p = self.alloc_layout(Layout::new::<T>()).cast::<T>();
        // SAFETY: fresh, aligned memory for one `T`, borrowed for as long as the arena is.
        unsafe {
            p.write(value);
            &mut *p.as_ptr()
        }
    }

    /// Room for `len` values, uninitialized.
    #[inline]
    #[expect(clippy::mut_from_ref, reason = "each allocation is disjoint")]
    pub fn alloc_uninit_slice<T>(&self, len: usize) -> &mut [MaybeUninit<T>] {
        let layout =
            Layout::array::<T>(len).unwrap_or_else(|_| exhausted(self, Layout::new::<T>()));
        let p = self.alloc_layout(layout).cast::<MaybeUninit<T>>();
        // SAFETY: fresh, aligned memory for `len` values; `MaybeUninit` needs no initialization.
        unsafe { core::slice::from_raw_parts_mut(p.as_ptr(), len) }
    }

    /// A copy of `src`.
    #[inline]
    #[expect(clippy::mut_from_ref, reason = "each allocation is disjoint")]
    pub fn alloc_slice_copy<T: Copy>(&self, src: &[T]) -> &mut [T] {
        let dst = self.alloc_uninit_slice::<T>(src.len());
        // SAFETY: `dst` is fresh memory of the same length; `T: Copy`.
        unsafe {
            ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr().cast::<T>(), src.len());
            &mut *(ptr::from_mut(dst) as *mut [T])
        }
    }

    /// `len` values made by `f(index)`.
    #[inline]
    #[expect(clippy::mut_from_ref, reason = "each allocation is disjoint")]
    pub fn alloc_slice_fill_with<T>(&self, len: usize, mut f: impl FnMut(usize) -> T) -> &mut [T] {
        let dst = self.alloc_uninit_slice::<T>(len);
        for (i, slot) in dst.iter_mut().enumerate() {
            slot.write(f(i));
        }
        // SAFETY: every element was just initialized. (Should `f` panic, the engine aborts.)
        unsafe { &mut *(ptr::from_mut(dst) as *mut [T]) }
    }

    /// A copy of `s`.
    #[inline]
    #[expect(clippy::mut_from_ref, reason = "each allocation is disjoint")]
    pub fn alloc_str(&self, s: &str) -> &mut str {
        let bytes = self.alloc_slice_copy(s.as_bytes());
        // SAFETY: a copy of valid UTF-8.
        unsafe { core::str::from_utf8_unchecked_mut(bytes) }
    }

    /// Releases everything allocated, keeping the committed memory for reuse.
    pub fn reset(&mut self) {
        // SAFETY: `&mut self` means no allocation is borrowed any more.
        unsafe { self.reset_shared() }
    }

    /// [`reset`](Self::reset) through a shared reference, for arenas whose users are tracked by
    /// other means ([`FrameArenas`]).
    ///
    /// # Safety
    /// Nothing allocated from the arena is used afterwards, and no thread allocates from it
    /// concurrently.
    pub unsafe fn reset_shared(&self) {
        // SAFETY: forwarded from the caller.
        unsafe { self.rewind(Checkpoint(0)) }
    }

    /// The current position, to [`rewind`](Self::rewind) to later.
    #[inline]
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint(self.used())
    }

    /// Releases everything allocated since `checkpoint`.
    ///
    /// # Safety
    /// Nothing allocated since `checkpoint` is used afterwards, no thread allocates from the arena
    /// concurrently, and `checkpoint` was taken from this arena since its last reset.
    pub unsafe fn rewind(&self, checkpoint: Checkpoint) {
        let used = self.used.load(Ordering::Relaxed);
        debug_assert!(checkpoint.0 <= used, "rewinding past the arena's end");
        self.peak.fetch_max(used, Ordering::Relaxed);
        #[cfg(debug_assertions)]
        {
            // Up to what is committed: a failed commit can leave `used` past it.
            let end = used.min(self.committed());
            if checkpoint.0 < end {
                // SAFETY: `checkpoint..end` is committed arena memory no one uses (the caller).
                unsafe {
                    ptr::write_bytes(
                        self.base.add(checkpoint.0).as_ptr(),
                        POISON,
                        end - checkpoint.0,
                    )
                };
            }
        }
        self.used.store(checkpoint.0, Ordering::Relaxed);
    }

    /// Runs `f` with this arena, then releases everything it allocated: stack-style use. The
    /// borrow keeps those allocations from escaping `f`.
    pub fn scope<R>(&mut self, f: impl for<'a> FnOnce(&'a Arena) -> R) -> R {
        let checkpoint = self.checkpoint();
        let result = f(self);
        // SAFETY: `&mut self` excludes other users, and `f` cannot return its allocations.
        unsafe { self.rewind(checkpoint) };
        result
    }

    /// Decommits memory beyond `keep` bytes (rounded up to the granule), or beyond the most used
    /// since the last trim with `None`: lets an arena shrink back after a spike.
    pub fn trim(&mut self, keep: Option<usize>) {
        let used = *self.used.get_mut();
        let peak = core::mem::replace(self.peak.get_mut(), used).max(used);
        let keep = keep
            .unwrap_or(peak)
            .max(used)
            .next_multiple_of(COMMIT_GRANULE)
            .min(self.reserved);
        let committed = *self.committed.get_mut();
        if keep < committed {
            // SAFETY: `keep..committed` is granule-aligned, committed, and unused (`&mut self`).
            if unsafe { vmem::decommit(self.base.add(keep), committed - keep) }.is_ok() {
                *self.committed.get_mut() = keep;
            }
        }
    }

    /// Whether `ptr..ptr + size` is the arena's last allocation.
    fn is_last(&self, ptr: NonNull<u8>, size: usize) -> bool {
        ptr.as_ptr() as usize + size == self.base.as_ptr() as usize + self.used()
    }

    /// Grows (or shrinks) the last allocation in place.
    fn resize_last(&self, ptr: NonNull<u8>, old: usize, new: usize) -> bool {
        let start = ptr.as_ptr() as usize - self.base.as_ptr() as usize;
        let Some(end) = start.checked_add(new).filter(|&e| e <= self.reserved) else {
            return false;
        };
        let moved = self
            .used
            .compare_exchange(start + old, end, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok();
        moved && (end <= self.committed.load(Ordering::Acquire) || self.commit_to(end).is_some())
    }
}

#[cold]
#[inline(never)]
fn exhausted(arena: &Arena, layout: Layout) -> ! {
    panic!(
        "arena exhausted: {} of {} bytes used, {} more requested",
        arena.used(),
        arena.capacity(),
        layout.size()
    )
}

impl Drop for Arena {
    fn drop(&mut self) {
        // SAFETY: the reservation is ours and nothing borrows from the arena any more.
        let _ = unsafe { vmem::release(self.base, self.reserved) };
    }
}

impl fmt::Debug for Arena {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Arena")
            .field("used", &self.used())
            .field("committed", &self.committed())
            .field("capacity", &self.capacity())
            .finish()
    }
}

// SAFETY: blocks are disjoint and live until the arena is reset, which needs `&mut Arena` (or
// the caller's promise in `reset_shared`), so no borrow of `&Arena` outlives them. `deallocate`
// only reclaims the last block, through the same atomic as allocation.
unsafe impl Allocator for &Arena {
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let p = self.try_alloc_layout(layout).ok_or(AllocError)?;
        Ok(NonNull::slice_from_raw_parts(p, layout.size()))
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // Reclaim the last block; anything else waits for the reset.
        if self.is_last(ptr, layout.size()) {
            let start = ptr.as_ptr() as usize - self.base.as_ptr() as usize;
            let _ = self.used.compare_exchange(
                start + layout.size(),
                start,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old: Layout,
        new: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        if new.align() <= old.align()
            && (ptr.as_ptr() as usize).is_multiple_of(new.align())
            && self.is_last(ptr, old.size())
            && self.resize_last(ptr, old.size(), new.size())
        {
            return Ok(NonNull::slice_from_raw_parts(ptr, new.size()));
        }
        let to = self.allocate(new)?;
        // SAFETY: `ptr` holds `old.size()` bytes; the new block is fresh and larger.
        unsafe { ptr::copy_nonoverlapping(ptr.as_ptr(), to.cast().as_ptr(), old.size()) };
        Ok(to)
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old: Layout,
        new: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        if (ptr.as_ptr() as usize).is_multiple_of(new.align()) {
            if self.is_last(ptr, old.size()) {
                self.resize_last(ptr, old.size(), new.size());
            }
            return Ok(NonNull::slice_from_raw_parts(ptr, new.size()));
        }
        let to = self.allocate(new)?;
        // SAFETY: the new block is fresh and holds `new.size()` bytes, all inside the old one.
        unsafe { ptr::copy_nonoverlapping(ptr.as_ptr(), to.cast().as_ptr(), new.size()) };
        Ok(to)
    }
}
