//! The table of live allocations: open addressing with linear probing, in memory straight from
//! the OS so recording an allocation never allocates.

use core::ptr::{self, NonNull, null_mut};

use marlin_pal::{Protection, vmem};

use super::MAX_FRAMES;
use crate::lock::Locked;
use crate::segment::mix;

#[derive(Clone, Copy)]
pub(crate) struct Entry {
    /// The block's address; 0 marks an empty slot.
    pub(crate) addr: usize,
    pub(crate) size: usize,
    /// The block's guard-page reservation, or null for a heap block.
    pub(crate) guard_base: *mut u8,
    pub(crate) guard_len: usize,
    pub(crate) frame_count: usize,
    pub(crate) frames: [usize; MAX_FRAMES],
}

impl Entry {
    pub(crate) const EMPTY: Entry = Entry {
        addr: 0,
        size: 0,
        guard_base: null_mut(),
        guard_len: 0,
        frame_count: 0,
        frames: [0; MAX_FRAMES],
    };

    #[inline]
    pub(crate) fn frames(&self) -> &[usize] {
        &self.frames[..self.frame_count]
    }
}

pub(crate) enum InsertError {
    /// The address is already live: the heap handed out a block twice.
    Duplicate,
    OutOfMemory,
}

pub(crate) struct Table {
    slots: *mut Entry,
    /// A power of two, or 0 before the first insert.
    capacity: usize,
    /// Bytes mapped for `slots`.
    mapped: usize,
    len: usize,
    bytes: usize,
}

pub(crate) static TABLE: Locked<Table> = Locked::new(Table {
    slots: null_mut(),
    capacity: 0,
    mapped: 0,
    len: 0,
    bytes: 0,
});

const MIN_CAPACITY: usize = 1024;

impl Table {
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    #[inline]
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    #[inline]
    fn home(&self, addr: usize) -> usize {
        mix(addr as u64) as usize & (self.capacity - 1)
    }

    /// # Safety
    /// `i < capacity`.
    #[inline]
    pub(crate) unsafe fn slot(&self, i: usize) -> *mut Entry {
        // SAFETY: the slots are mapped for `capacity` entries.
        unsafe { self.slots.add(i) }
    }

    fn find(&self, addr: usize) -> Option<usize> {
        if self.capacity == 0 {
            return None;
        }
        let mut i = self.home(addr);
        loop {
            // SAFETY: `i` is masked below the capacity; the table is never full.
            let slot = unsafe { (*self.slot(i)).addr };
            if slot == addr {
                return Some(i);
            }
            if slot == 0 {
                return None;
            }
            i = (i + 1) & (self.capacity - 1);
        }
    }

    pub(crate) fn insert(&mut self, entry: &Entry) -> Result<(), InsertError> {
        // At most 3/4 full, so probes stay short and always find an empty slot.
        if (self.len + 1) * 4 > self.capacity * 3 && !self.grow() {
            return Err(InsertError::OutOfMemory);
        }
        let mut i = self.home(entry.addr);
        // SAFETY: indices are masked below the capacity, and there is an empty slot.
        unsafe {
            loop {
                let slot = self.slot(i);
                if (*slot).addr == entry.addr {
                    return Err(InsertError::Duplicate);
                }
                if (*slot).addr == 0 {
                    *slot = *entry;
                    break;
                }
                i = (i + 1) & (self.capacity - 1);
            }
        }
        self.len += 1;
        self.bytes += entry.size;
        Ok(())
    }

    pub(crate) fn remove(&mut self, addr: usize) -> Option<Entry> {
        let mut hole = self.find(addr)?;
        let mask = self.capacity - 1;
        // SAFETY: indices are masked below the capacity.
        let removed = unsafe { *self.slot(hole) };
        // Backward-shift deletion: move later entries of the probe run into the hole unless their
        // home lies cyclically in (hole, j].
        let mut j = hole;
        // SAFETY: as above.
        unsafe {
            loop {
                j = (j + 1) & mask;
                let entry = self.slot(j);
                if (*entry).addr == 0 {
                    break;
                }
                let home = self.home((*entry).addr);
                let stays = if hole <= j {
                    hole < home && home <= j
                } else {
                    hole < home || home <= j
                };
                if !stays {
                    *self.slot(hole) = *entry;
                    hole = j;
                }
            }
            (*self.slot(hole)).addr = 0;
        }
        self.len -= 1;
        self.bytes -= removed.size;
        Some(removed)
    }

    /// Doubles the capacity, rehashing into fresh pages.
    #[cold]
    fn grow(&mut self) -> bool {
        let capacity = (self.capacity * 2).max(MIN_CAPACITY);
        let Some(mapped) = capacity
            .checked_mul(size_of::<Entry>())
            .and_then(|bytes| bytes.checked_next_multiple_of(vmem::allocation_granularity()))
        else {
            return false;
        };
        let Ok(base) = vmem::reserve(mapped) else {
            return false;
        };
        // SAFETY: commits the reservation just made.
        if unsafe { vmem::commit(base, mapped, Protection::ReadWrite) }.is_err() {
            // SAFETY: releases the reservation just made.
            let _ = unsafe { vmem::release(base, mapped) };
            return false;
        }
        // Fresh pages are zero: every slot empty.
        let old = core::mem::replace(
            self,
            Table {
                slots: base.as_ptr().cast(),
                capacity,
                mapped,
                len: 0,
                bytes: 0,
            },
        );
        for i in 0..old.capacity {
            // SAFETY: `i` is below the old capacity.
            let entry = unsafe { &*old.slot(i) };
            if entry.addr != 0 {
                // Cannot fail: the new table is twice as large and has no duplicates.
                let _ = self.insert(entry);
            }
        }
        if let Some(slots) = NonNull::new(old.slots.cast::<u8>()) {
            // SAFETY: the old slots' reservation, no longer referenced.
            let _ = unsafe { vmem::release(slots, old.mapped) };
        }
        true
    }
}

/// Copies up to `out.len()` live entries at slot `*cursor` or after into `out`, advancing
/// `*cursor`. Returns how many were copied, 0 at the end.
pub(crate) fn next_batch(cursor: &mut usize, out: &mut [Entry]) -> usize {
    TABLE.with(|table| {
        let mut n = 0;
        while *cursor < table.capacity() && n < out.len() {
            // SAFETY: the cursor is below the capacity.
            let entry = unsafe { &*table.slot(*cursor) };
            if entry.addr != 0 {
                // SAFETY: a plain copy into the caller's buffer.
                unsafe { ptr::write(&mut out[n], *entry) };
                n += 1;
            }
            *cursor += 1;
        }
        n
    })
}
