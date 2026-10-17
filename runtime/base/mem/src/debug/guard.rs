//! The guard-page allocator: every block in pages of its own, ending at an inaccessible page.

use core::ptr::NonNull;

use marlin_pal::{Protection, vmem};

use crate::layout::SEGMENT_SIZE;
use crate::lock::Locked;

/// Freed guard blocks kept reserved (but decommitted) before their address space is released, so
/// a use after free faults instead of hitting a new block at the same address.
pub const QUARANTINE: usize = 1024;

struct Quarantine {
    slots: [(*mut u8, usize); QUARANTINE],
    /// The oldest entry.
    head: usize,
    len: usize,
}

static QUARANTINED: Locked<Quarantine> = Locked::new(Quarantine {
    slots: [(core::ptr::null_mut(), 0); QUARANTINE],
    head: 0,
    len: 0,
});

pub(crate) struct GuardBlock {
    pub(crate) ptr: NonNull<u8>,
    /// The reservation holding it.
    pub(crate) base: NonNull<u8>,
    pub(crate) len: usize,
}

/// A block of `size` bytes in a reservation of its own: committed pages holding the block as
/// close to their end as `align` allows, then one reserved, inaccessible page.
pub(crate) fn alloc(size: usize, align: usize) -> Option<GuardBlock> {
    // Like the heap.
    if align > SEGMENT_SIZE / 2 {
        return None;
    }
    let page = vmem::page_size();
    let size = size.max(1);
    let data = size.checked_next_multiple_of(page)?;
    let len = data.checked_add(page)?;
    let base = if align > vmem::allocation_granularity() {
        vmem::reserve_aligned(len, align)
    } else {
        vmem::reserve(len)
    }
    .ok()?;
    // SAFETY: the data pages lie at the start of the reservation just made.
    if unsafe { vmem::commit(base, data, Protection::ReadWrite) }.is_err() {
        // SAFETY: releases the reservation just made.
        let _ = unsafe { vmem::release(base, len) };
        return None;
    }
    // The base is aligned to `align`, so rounding down stays inside the data pages.
    let start = (base.addr().get() + data - size) & !(align - 1);
    Some(GuardBlock {
        ptr: base.with_addr(core::num::NonZero::new(start)?),
        base,
        len,
    })
}

/// Makes a freed block's pages inaccessible and quarantines them, releasing the oldest
/// quarantined block once the quarantine is full.
///
/// # Safety
/// `base` and `len` describe a block from [`alloc`] that is no longer used.
pub(crate) unsafe fn free(base: NonNull<u8>, len: usize) {
    let data = len - vmem::page_size();
    // SAFETY: the block's data pages, unused from now on. Should decommitting fail, the pages
    // stay accessible: the block is still quarantined.
    let _ = unsafe { vmem::decommit(base, data) };
    let evicted = QUARANTINED.with(|q| {
        if q.len < QUARANTINE {
            q.slots[(q.head + q.len) % QUARANTINE] = (base.as_ptr(), len);
            q.len += 1;
            None
        } else {
            let oldest = core::mem::replace(&mut q.slots[q.head], (base.as_ptr(), len));
            q.head = (q.head + 1) % QUARANTINE;
            Some(oldest)
        }
    });
    if let Some((base, len)) = evicted {
        // SAFETY: a whole guard reservation, out of quarantine and referenced by nothing.
        let _ = unsafe { vmem::release(NonNull::new_unchecked(base), len) };
    }
}
