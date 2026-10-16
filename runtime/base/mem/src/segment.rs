//! Segments and pages: the allocator's memory layout.
//!
//! A segment is a [`SEGMENT_SIZE`]-aligned reservation starting with its header, so the segment
//! of any pointer is found by masking. Small and medium segments are split into equal pages
//! (64 KiB / 512 KiB) that each serve one size class. A huge segment holds one allocation and may
//! span several segment sizes.
//!
//! # Concurrency
//!
//! A small or medium segment is owned by one heap, i.e. one thread, and only that thread touches
//! the owner-side fields. Other threads reach a page only through a block they own, so they only
//! push onto [`Page::thread_free`], use the other atomics, and read fields that cannot change
//! while the page has a live block (`kind`, `page_shift`, `bin`, `block_size`, `area`).

use core::ptr::{self, null_mut};
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

use marlin_pal::{Protection, vmem};

use crate::heap::Heap;
use crate::layout::{MAX_PAGES, MEDIUM_PAGE_SHIFT, SEGMENT_SIZE, SMALL_PAGE_SHIFT, bin_size};
use crate::lock::Locked;

pub(crate) struct Block {
    pub(crate) next: *mut Block,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Kind {
    Small = 0,
    Medium = 1,
    Huge = 2,
}

#[repr(C)]
pub(crate) struct Page {
    /// Blocks ready to hand out.
    pub(crate) free: *mut Block,
    /// Blocks freed by the owner. They become `free` once it runs dry, so the fast paths of
    /// allocation and free never touch the same list.
    pub(crate) local_free: *mut Block,
    pub(crate) next: *mut Page,
    pub(crate) prev: *mut Page,
    /// Blocks handed out and not yet back with the owner (foreign frees count once collected).
    pub(crate) used: u32,
    /// Blocks carved out so far. Pages are carved lazily so memory is touched gradually.
    pub(crate) capacity: u32,
    /// Blocks that fit in the page.
    pub(crate) reserved: u32,
    pub(crate) in_use: bool,
    pub(crate) committed: bool,
    pub(crate) bin: u8,
    pub(crate) block_size: usize,
    /// First block; for huge segments, the allocation.
    pub(crate) area: *mut u8,
    /// Blocks freed by other threads, collected by the owner.
    pub(crate) thread_free: AtomicPtr<Block>,
    /// The page is in its heap's full queue: a foreign free must leave a hint.
    pub(crate) in_full: AtomicBool,
    /// Some block was handed out at an interior pointer (over-aligned allocation).
    pub(crate) has_aligned: AtomicBool,
}

#[repr(C)]
pub(crate) struct Segment {
    /// Owning heap; null for huge, abandoned and cached segments.
    pub(crate) heap: AtomicPtr<Heap>,
    pub(crate) kind: Kind,
    pub(crate) page_shift: u32,
    pub(crate) page_count: usize,
    pub(crate) used_pages: usize,
    /// Reservation size: SEGMENT_SIZE, or a multiple of it for big huge allocations.
    pub(crate) size: usize,
    /// Huge segments: bytes committed from the segment start.
    pub(crate) committed: usize,
    /// In the owner's queue of segments with unused pages.
    pub(crate) in_free_queue: bool,
    pub(crate) free_next: *mut Segment,
    pub(crate) free_prev: *mut Segment,
    /// In the owner's list of all its segments, or in the abandoned list.
    pub(crate) owned_next: *mut Segment,
    pub(crate) owned_prev: *mut Segment,
    pub(crate) pages: [Page; MAX_PAGES],
}

/// Bytes at the start of every segment reserved for the header; page 0's blocks follow it.
pub(crate) const HEADER_SIZE: usize = size_of::<Segment>().next_multiple_of(4096);
const _: () = assert!(HEADER_SIZE <= (1 << SMALL_PAGE_SHIFT) / 4);

/// Blocks carved per page extension: about one OS page of memory.
const EXTEND_BYTES: usize = 4096;

/// Free segments kept for reuse, header committed and everything else decommitted.
const CACHE_CAPACITY: usize = 16;

struct Cache {
    len: usize,
    slots: [*mut u8; CACHE_CAPACITY],
}

static CACHE: Locked<Cache> = Locked::new(Cache {
    len: 0,
    slots: [null_mut(); CACHE_CAPACITY],
});

/// Bytes reserved from the OS for segments, including cached ones.
pub(crate) static RESERVED_BYTES: AtomicUsize = AtomicUsize::new(0);

#[inline]
pub(crate) fn segment_of<T>(p: *const T) -> *mut Segment {
    p.cast::<u8>()
        .cast_mut()
        .map_addr(|a| a & !(SEGMENT_SIZE - 1))
        .cast()
}

/// # Safety
/// `p` points into a live small or medium segment `seg`.
#[inline]
pub(crate) unsafe fn page_of(seg: *mut Segment, p: *const u8) -> *mut Page {
    // SAFETY: `page_shift` is immutable while `p` is live, and the index is below `page_count`.
    unsafe {
        let index = (p.addr() - seg.addr()) >> (*seg).page_shift;
        (&raw mut (*seg).pages).cast::<Page>().add(index)
    }
}

/// The block containing `p`, for pages that hand out interior pointers.
///
/// # Safety
/// `p` points into a live block of `page`.
#[inline]
pub(crate) unsafe fn block_start(page: *mut Page, p: *mut u8) -> *mut Block {
    // SAFETY: `area` and `block_size` are immutable while the page has a live block.
    unsafe {
        let area = (*page).area;
        let size = (*page).block_size;
        area.add((p.addr() - area.addr()) / size * size).cast()
    }
}

/// The header's committed size, whole OS pages.
fn header_commit() -> usize {
    HEADER_SIZE.next_multiple_of(vmem::page_size())
}

/// Reserves an aligned segment of `size` bytes with its header committed.
fn os_reserve(size: usize) -> *mut u8 {
    let Ok(base) = vmem::reserve_aligned(size, SEGMENT_SIZE) else {
        return null_mut();
    };
    // SAFETY: the header lies at the start of the reservation just made.
    if unsafe { vmem::commit(base, header_commit(), Protection::ReadWrite) }.is_err() {
        // SAFETY: releases the reservation just made.
        let _ = unsafe { vmem::release(base, size) };
        return null_mut();
    }
    RESERVED_BYTES.fetch_add(size, Ordering::Relaxed);
    base.as_ptr()
}

fn cache_pop() -> *mut u8 {
    CACHE.with(|cache| {
        if cache.len == 0 {
            null_mut()
        } else {
            cache.len -= 1;
            cache.slots[cache.len]
        }
    })
}

pub(crate) fn cached_segments() -> usize {
    CACHE.with(|cache| cache.len)
}

/// Writes a fresh header at `base`, whose header is committed.
///
/// # Safety
/// `base` is a reservation of `size` bytes nobody else uses.
unsafe fn segment_init(base: *mut u8, kind: Kind, size: usize) -> *mut Segment {
    let seg = base.cast::<Segment>();
    // SAFETY: the header is committed and unused; all-zero is a valid header (null pointers,
    // `Kind::Small`, unused pages).
    unsafe {
        ptr::write_bytes(seg, 0, 1);
        let shift = if kind == Kind::Medium {
            MEDIUM_PAGE_SHIFT
        } else {
            SMALL_PAGE_SHIFT
        };
        (*seg).kind = kind;
        (*seg).page_shift = shift;
        (*seg).page_count = if kind == Kind::Huge {
            1
        } else {
            SEGMENT_SIZE >> shift
        };
        (*seg).size = size;
    }
    seg
}

/// A new small or medium segment with no pages in use and no owner, or null when out of memory.
pub(crate) fn segment_alloc(kind: Kind) -> *mut Segment {
    let mut base = cache_pop();
    if base.is_null() {
        base = os_reserve(SEGMENT_SIZE);
        if base.is_null() {
            return null_mut();
        }
    }
    // SAFETY: `base` is a segment-sized reservation we own.
    unsafe { segment_init(base, kind, SEGMENT_SIZE) }
}

/// Returns a segment with no live blocks to the cache or the OS.
///
/// # Safety
/// No live block remains in `seg`, and no list references it.
pub(crate) unsafe fn segment_release(seg: *mut Segment) {
    let base = seg.cast::<u8>();
    // SAFETY: the caller guarantees nobody uses the segment any more.
    unsafe {
        let size = (*seg).size;
        if size == SEGMENT_SIZE {
            // Decommit everything but the header; on reuse pages are committed again as needed,
            // and huge allocations get zeroed memory for free.
            let commit = header_commit();
            let tail = ptr::NonNull::new_unchecked(base.add(commit));
            if vmem::decommit(tail, SEGMENT_SIZE - commit).is_ok() {
                let cached = CACHE.with(|cache| {
                    let room = cache.len < CACHE_CAPACITY;
                    if room {
                        cache.slots[cache.len] = base;
                        cache.len += 1;
                    }
                    room
                });
                if cached {
                    return;
                }
            }
        }
        RESERVED_BYTES.fetch_sub(size, Ordering::Relaxed);
        let _ = vmem::release(ptr::NonNull::new_unchecked(base), size);
    }
}

/// Allocates `size` bytes aligned to `align` in a segment of its own. Huge segments have no
/// owner; any thread frees them directly.
pub(crate) fn huge_alloc(size: usize, align: usize) -> *mut u8 {
    // The block must start in the first SEGMENT_SIZE bytes so masking finds the header.
    if align > SEGMENT_SIZE / 2 {
        return null_mut();
    }
    let offset = HEADER_SIZE.next_multiple_of(align);
    let Some(end) = offset.checked_add(size.max(1)) else {
        return null_mut();
    };
    let Some(reserve) = end.checked_next_multiple_of(SEGMENT_SIZE) else {
        return null_mut();
    };
    let mut base = if reserve == SEGMENT_SIZE {
        cache_pop()
    } else {
        null_mut()
    };
    if base.is_null() {
        base = os_reserve(reserve);
        if base.is_null() {
            return null_mut();
        }
    }

    // SAFETY: `base` is a reservation of `reserve` bytes we own, header committed.
    unsafe {
        let seg = segment_init(base, Kind::Huge, reserve);
        let committed = end.next_multiple_of(vmem::page_size());
        let start = ptr::NonNull::new_unchecked(base);
        if vmem::commit(start, committed, Protection::ReadWrite).is_err() {
            segment_release(seg);
            return null_mut();
        }
        (*seg).committed = committed;
        let p = base.add(offset);
        (*seg).pages[0].area = p;
        p
    }
}

/// Grows the huge allocation at `p` in place to `new_size` bytes, if its reservation allows.
///
/// # Safety
/// `p` is a live allocation owned by the caller.
pub(crate) unsafe fn huge_grow(p: *mut u8, new_size: usize) -> bool {
    let seg = segment_of(p);
    // SAFETY: the caller owns the only block of a huge segment, hence the whole segment.
    unsafe {
        if (*seg).kind != Kind::Huge {
            return false;
        }
        let Some(end) = (p.addr() - seg.addr()).checked_add(new_size) else {
            return false;
        };
        if end > (*seg).size {
            return false;
        }
        let committed = (*seg).committed;
        let end = end.next_multiple_of(vmem::page_size());
        if end > committed {
            let start = ptr::NonNull::new_unchecked(seg.cast::<u8>().add(committed));
            if vmem::commit(start, end - committed, Protection::ReadWrite).is_err() {
                return false;
            }
            (*seg).committed = end;
        }
        true
    }
}

/// Bytes usable from `p`, which may be past the requested size.
///
/// # Safety
/// `p` is a live allocation.
pub(crate) unsafe fn usable_size(p: *mut u8) -> usize {
    let seg = segment_of(p);
    // SAFETY: the fields read are immutable while `p` is live.
    unsafe {
        if (*seg).kind == Kind::Huge {
            return (*seg).committed - (p.addr() - seg.addr());
        }
        let page = page_of(seg, p);
        let start = if (*page).has_aligned.load(Ordering::Relaxed) {
            block_start(page, p).cast()
        } else {
            p
        };
        (*page).block_size - (p.addr() - start.addr())
    }
}

/// Prepares unused page `index` of `seg` for blocks of `bin`. Returns null when out of memory.
///
/// # Safety
/// The calling thread owns `seg`, and page `index` is unused.
pub(crate) unsafe fn page_init(seg: *mut Segment, index: usize, bin: usize) -> *mut Page {
    // SAFETY: the caller owns the segment and the page is unused, so no other thread looks at it.
    unsafe {
        let page = (&raw mut (*seg).pages).cast::<Page>().add(index);
        let shift = (*seg).page_shift;
        let page_start = seg.cast::<u8>().add(index << shift);
        if !(*page).committed {
            let start = ptr::NonNull::new_unchecked(page_start);
            if vmem::commit(start, 1 << shift, Protection::ReadWrite).is_err() {
                return null_mut();
            }
            (*page).committed = true;
        }
        let area = if index == 0 {
            seg.cast::<u8>().add(HEADER_SIZE)
        } else {
            page_start
        };
        let usable = (1usize << shift) - (area.addr() - page_start.addr());
        let block_size = bin_size(bin);

        (*page).free = null_mut();
        (*page).local_free = null_mut();
        (*page).next = null_mut();
        (*page).prev = null_mut();
        (*page).used = 0;
        (*page).capacity = 0;
        (*page).reserved = (usable / block_size) as u32;
        (*page).in_use = true;
        (*page).bin = bin as u8;
        (*page).block_size = block_size;
        (*page).area = area;
        (*page).thread_free.store(null_mut(), Ordering::Relaxed);
        (*page).in_full.store(false, Ordering::Relaxed);
        (*page).has_aligned.store(false, Ordering::Relaxed);
        page
    }
}

/// Carves the next run of blocks into the empty free list.
///
/// # Safety
/// The calling thread owns `page`, its `free` list is empty and `capacity < reserved`.
pub(crate) unsafe fn page_extend(page: *mut Page) {
    // SAFETY: the blocks carved lie in the committed page area past those handed out so far.
    unsafe {
        let size = (*page).block_size;
        let count = (EXTEND_BYTES / size)
            .max(1)
            .min(((*page).reserved - (*page).capacity) as usize);
        let first = (*page).area.add((*page).capacity as usize * size);
        let mut block = first;
        for _ in 1..count {
            let next = block.add(size);
            (*block.cast::<Block>()).next = next.cast();
            block = next;
        }
        (*block.cast::<Block>()).next = null_mut();
        (*page).free = first.cast();
        (*page).capacity += count as u32;
    }
}

/// Moves blocks freed by other threads, and by the owner when `free` is empty, into `free`.
///
/// # Safety
/// The calling thread owns `page`.
pub(crate) unsafe fn page_collect(page: *mut Page) {
    // SAFETY: the owner has exclusive use of the lists; the swap hands it the foreign list.
    unsafe {
        let mut foreign = (*page).thread_free.swap(null_mut(), Ordering::Acquire);
        if !foreign.is_null() {
            let mut count = 1;
            let head = foreign;
            while !(*foreign).next.is_null() {
                foreign = (*foreign).next;
                count += 1;
            }
            (*foreign).next = (*page).local_free;
            (*page).local_free = head;
            debug_assert!((*page).used >= count, "double free");
            (*page).used -= count;
        }
        if (*page).free.is_null() {
            (*page).free = (*page).local_free;
            (*page).local_free = null_mut();
        }
    }
}

/// Takes a block from `free`.
///
/// # Safety
/// The calling thread owns `page`, and `free` is not empty.
#[inline]
pub(crate) unsafe fn page_pop(page: *mut Page) -> *mut u8 {
    // SAFETY: `free` holds valid blocks of this page.
    unsafe {
        let block = (*page).free;
        (*page).free = (*block).next;
        (*page).used += 1;
        block.cast()
    }
}
