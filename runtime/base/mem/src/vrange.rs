//! [`Reservation`]: a reserved address range with page-granular commit tracking.

use core::fmt;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};

use marlin_pal::{Error, ErrorKind, Protection, vmem};

use crate::lock::Lock;

/// A range of reserved address space, released on drop, whose pages are committed and
/// decommitted individually. Tracks which pages are committed, so [`committed`](Self::committed)
/// is exact however ranges overlap.
///
/// Ranges are whole pages: offsets must be multiples of [`page_size`](vmem::page_size) and
/// lengths are rounded up to it. Any thread may commit or decommit; calls are serialized.
///
/// The commit bitmap (one bit per page) lives in a reservation of its own whose pages are
/// committed as the bits they hold are first needed.
pub struct Reservation {
    base: NonNull<u8>,
    size: usize,
    page: usize,
    bits: NonNull<u64>,
    bits_size: usize,
    committed: AtomicUsize,
    lock: Lock,
}

// SAFETY: the reservation is owned; the bitmap is only touched under `lock`.
unsafe impl Send for Reservation {}
// SAFETY: as above.
unsafe impl Sync for Reservation {}

fn invalid() -> Error {
    Error::from_kind(ErrorKind::InvalidInput)
}

impl Reservation {
    /// Reserves at least `size` bytes, rounded up to the [allocation
    /// granularity](vmem::allocation_granularity) and aligned to it. Nothing is committed.
    pub fn new(size: usize) -> marlin_pal::Result<Self> {
        Self::with_alignment(size, 1)
    }

    /// Like [`new`](Self::new), with the base aligned to `align` (a power of two).
    pub fn with_alignment(size: usize, align: usize) -> marlin_pal::Result<Self> {
        let size = size
            .max(1)
            .checked_next_multiple_of(vmem::allocation_granularity())
            .ok_or_else(invalid)?;
        let page = vmem::page_size();
        let bits_size = (size / page)
            .div_ceil(64)
            .saturating_mul(8)
            .next_multiple_of(vmem::allocation_granularity());
        let base = vmem::reserve_aligned(size, align)?;
        let bits = match vmem::reserve(bits_size) {
            Ok(bits) => bits.cast(),
            Err(error) => {
                // SAFETY: releases the reservation just made.
                let _ = unsafe { vmem::release(base, size) };
                return Err(error);
            }
        };
        Ok(Self {
            base,
            size,
            page,
            bits,
            bits_size,
            committed: AtomicUsize::new(0),
            lock: Lock::new(),
        })
    }

    #[inline]
    pub fn base(&self) -> NonNull<u8> {
        self.base
    }

    #[inline]
    pub fn as_ptr(&self) -> *mut u8 {
        self.base.as_ptr()
    }

    /// Bytes reserved.
    #[inline]
    pub fn size(&self) -> usize {
        self.size
    }

    /// Bytes committed.
    #[inline]
    pub fn committed(&self) -> usize {
        self.committed.load(Ordering::Relaxed)
    }

    /// Whether `ptr` lies in the reservation.
    #[inline]
    pub fn contains(&self, ptr: *const u8) -> bool {
        ptr.addr().wrapping_sub(self.base.addr().get()) < self.size
    }

    /// Commits the pages of `offset..offset + len` with `prot`. Fresh pages read as zero; pages
    /// already committed keep their contents and take the new protection.
    pub fn commit(&self, offset: usize, len: usize, prot: Protection) -> marlin_pal::Result<()> {
        let (first, count) = self.pages(offset, len)?;
        self.locked(|| {
            self.map_bits(first, count)?;
            // SAFETY: whole pages inside the reservation.
            unsafe { vmem::commit(self.page_ptr(first), count * self.page, prot)? };
            // SAFETY: the bits' pages are committed, and we hold the lock.
            let newly = unsafe { self.update_bits(first, count, true) };
            self.committed
                .fetch_add(newly * self.page, Ordering::Relaxed);
            Ok(())
        })
    }

    /// Returns the pages of `offset..offset + len` to the OS. They read as zero once committed
    /// again.
    ///
    /// # Safety
    /// Nothing accesses the range until it is committed again.
    pub unsafe fn decommit(&self, offset: usize, len: usize) -> marlin_pal::Result<()> {
        let (first, count) = self.pages(offset, len)?;
        self.locked(|| {
            self.map_bits(first, count)?;
            // SAFETY: whole pages inside the reservation, unused (the caller).
            unsafe { vmem::decommit(self.page_ptr(first), count * self.page)? };
            // SAFETY: the bits' pages are committed, and we hold the lock.
            let cleared = unsafe { self.update_bits(first, count, false) };
            self.committed
                .fetch_sub(cleared * self.page, Ordering::Relaxed);
            Ok(())
        })
    }

    /// Changes the protection of the pages of `offset..offset + len`, which must all be
    /// committed (`InvalidInput` otherwise).
    ///
    /// # Safety
    /// No live reference relies on access the new protection removes.
    pub unsafe fn protect(
        &self,
        offset: usize,
        len: usize,
        prot: Protection,
    ) -> marlin_pal::Result<()> {
        let (first, count) = self.pages(offset, len)?;
        self.locked(|| {
            self.map_bits(first, count)?;
            // SAFETY: the bits' pages are committed, and we hold the lock.
            if unsafe { self.count_committed(first, count) } != count {
                return Err(invalid());
            }
            // SAFETY: committed pages inside the reservation; references are the caller's.
            unsafe { vmem::protect(self.page_ptr(first), count * self.page, prot) }
        })
    }

    /// Whether the page holding byte `offset` is committed.
    pub fn is_committed(&self, offset: usize) -> bool {
        if offset >= self.size {
            return false;
        }
        let page = offset / self.page;
        self.locked(|| {
            // SAFETY: the bits' pages are committed first, and we hold the lock.
            self.map_bits(page, 1).is_ok() && unsafe { self.count_committed(page, 1) } == 1
        })
    }

    /// The pages `offset..offset + len` covers, as (first page, count).
    fn pages(&self, offset: usize, len: usize) -> marlin_pal::Result<(usize, usize)> {
        let end = offset.checked_add(len).ok_or_else(invalid)?;
        if len == 0 || !offset.is_multiple_of(self.page) || end > self.size {
            return Err(invalid());
        }
        let first = offset / self.page;
        Ok((first, end.div_ceil(self.page) - first))
    }

    fn page_ptr(&self, page: usize) -> NonNull<u8> {
        // SAFETY: callers pass pages inside the reservation.
        unsafe { self.base.add(page * self.page) }
    }

    fn locked<R>(&self, f: impl FnOnce() -> R) -> R {
        self.lock.lock();
        let result = f();
        self.lock.unlock();
        result
    }

    /// Commits the bitmap pages holding the bits of pages `first..first + count`. Recommitting is
    /// harmless: committed pages keep their contents.
    fn map_bits(&self, first: usize, count: usize) -> marlin_pal::Result<()> {
        let start = (first / 64 * 8) / self.page * self.page;
        let end = ((first + count - 1) / 64 * 8 + 8).next_multiple_of(self.page);
        debug_assert!(end <= self.bits_size);
        // SAFETY: whole pages inside the bitmap's reservation.
        unsafe {
            vmem::commit(
                self.bits.cast::<u8>().add(start),
                end - start,
                Protection::ReadWrite,
            )
        }
    }

    /// Sets or clears the bits of pages `first..first + count`; returns how many changed.
    ///
    /// # Safety
    /// The lock is held, and [`map_bits`](Self::map_bits) succeeded for the range.
    unsafe fn update_bits(&self, first: usize, count: usize, set: bool) -> usize {
        let mut changed = 0;
        // SAFETY: the words are committed and only touched under the lock.
        unsafe {
            for_each_word(first, count, |word, mask| {
                let word = self.bits.add(word).as_ptr();
                let old = *word;
                let new = if set { old | mask } else { old & !mask };
                changed += (old ^ new).count_ones() as usize;
                *word = new;
            });
        }
        changed
    }

    /// How many of pages `first..first + count` are committed.
    ///
    /// # Safety
    /// As for [`update_bits`](Self::update_bits).
    unsafe fn count_committed(&self, first: usize, count: usize) -> usize {
        let mut committed = 0;
        // SAFETY: the words are committed and only touched under the lock.
        unsafe {
            for_each_word(first, count, |word, mask| {
                committed += (*self.bits.add(word).as_ptr() & mask).count_ones() as usize;
            });
        }
        committed
    }
}

/// Calls `f(word index, bit mask)` for the bitmap words covering bits `first..first + count`.
fn for_each_word(first: usize, count: usize, mut f: impl FnMut(usize, u64)) {
    let end = first + count;
    let mut bit = first;
    while bit < end {
        let word = bit / 64;
        let lo = bit % 64;
        let hi = (end - word * 64).min(64);
        let mask = if hi - lo == 64 {
            u64::MAX
        } else {
            ((1u64 << (hi - lo)) - 1) << lo
        };
        f(word, mask);
        bit = (word + 1) * 64;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        // SAFETY: both reservations are ours, and `&mut self` means no access through `self`
        // remains; memory handed out from it is the owner's to stop using.
        unsafe {
            let _ = vmem::release(self.base, self.size);
            let _ = vmem::release(self.bits.cast(), self.bits_size);
        }
    }
}

impl fmt::Debug for Reservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reservation")
            .field("base", &self.base)
            .field("size", &self.size)
            .field("committed", &self.committed())
            .finish()
    }
}
