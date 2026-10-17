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
//! while the page has a live block (`kind`, `page_shift`, `bin`, `block_size`, `area`, `keys`).
//!
//! # Free list encoding
//!
//! A free block's link is stored xored with a [mask](mask) derived from its page's keys and the
//! block's own address. The keys are fresh for every use of a page and derived from a
//! process-wide random seed, so a stray write of a plain pointer into a free block (overflow,
//! use-after-free) cannot steer the allocator to an address of the writer's choosing. Debug
//! builds check every decoded link and abort on corruption.

use core::ptr::{self, null_mut};
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

use marlin_pal::{Protection, time, vmem};

use crate::heap::Heap;
use crate::layout::{MAX_PAGES, MEDIUM_PAGE_SHIFT, SEGMENT_SIZE, SMALL_PAGE_SHIFT, bin_size};
use crate::lock::Locked;

/// A free block: the encoded link to the next one (see [`set_next`]).
pub(crate) struct Block {
    next: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Kind {
    Small = 0,
    Medium = 1,
    Huge = 2,
}

/// The fields the fast paths touch come first, within 42 bytes.
#[repr(C)]
pub(crate) struct Page {
    /// Blocks ready to hand out.
    pub(crate) free: *mut Block,
    /// Keys of the blocks' encoded links.
    pub(crate) keys: [usize; 2],
    /// Blocks freed by the owner. They become `free` once it runs dry, so the fast paths of
    /// allocation and free never touch the same list.
    pub(crate) local_free: *mut Block,
    /// Blocks handed out and not yet back with the owner (foreign frees count once collected).
    pub(crate) used: u32,
    /// Blocks carved out so far. Pages are carved lazily so memory is touched gradually.
    pub(crate) capacity: u32,
    /// The page is in its heap's full queue: a foreign free must leave a hint.
    pub(crate) in_full: AtomicBool,
    /// Some block was handed out at an interior pointer (over-aligned allocation).
    pub(crate) has_aligned: AtomicBool,
    pub(crate) in_use: bool,
    pub(crate) committed: bool,
    /// Unused and still committed: in the owner's retired queue until purged or reused.
    pub(crate) retired: bool,
    pub(crate) bin: u8,
    /// Blocks that fit in the page.
    pub(crate) reserved: u32,
    pub(crate) block_size: usize,
    /// First block; for huge segments, the allocation.
    pub(crate) area: *mut u8,
    /// Blocks freed by other threads, collected by the owner.
    pub(crate) thread_free: AtomicPtr<Block>,
    /// Links in the owner's queue for the bin, or in its retired queue.
    pub(crate) next: *mut Page,
    pub(crate) prev: *mut Page,
    /// When the page was retired, in `marlin_pal::time` ticks.
    pub(crate) retired_at: u64,
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
    /// Bytes committed, the header's included. For huge segments, a prefix of the segment.
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
/// Bytes committed in segments, including cached ones' headers.
pub(crate) static COMMITTED_BYTES: AtomicUsize = AtomicUsize::new(0);

// --- Free list encoding ---

/// Process-wide random seed of the page keys; 0 until first used.
static SEED: AtomicUsize = AtomicUsize::new(0);
/// Pages initialized so far, mixed into their keys.
static PAGE_INITS: AtomicUsize = AtomicUsize::new(0);

/// A 64-bit integer mixer with full avalanche (splitmix64's finalizer).
#[inline]
pub(crate) const fn mix(x: u64) -> u64 {
    let x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn seed() -> u64 {
    match SEED.load(Ordering::Relaxed) {
        0 => init_seed(),
        seed => seed as u64,
    }
}

/// There is no RNG at this level: the clock, mixed with stack, static and code addresses (which
/// ASLR randomizes where the OS has it).
#[cold]
fn init_seed() -> u64 {
    let local = 0u8;
    let entropy = time::ticks()
        ^ ((&raw const local).addr() as u64).rotate_left(13)
        ^ ((&raw const SEED).addr() as u64).rotate_left(29)
        ^ ((init_seed as fn() -> u64 as usize) as u64).rotate_left(47);
    let seed = mix(entropy) as usize | 1;
    match SEED.compare_exchange(0, seed, Ordering::Relaxed, Ordering::Relaxed) {
        Ok(_) => seed as u64,
        Err(winner) => winner as u64,
    }
}

/// Fresh keys for `page`.
fn page_keys(page: *mut Page) -> [usize; 2] {
    let n = PAGE_INITS.fetch_add(1, Ordering::Relaxed) as u64;
    let seed = seed();
    let k0 = mix(seed ^ page.addr() as u64 ^ n.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    // Odd, so the multiplication is a bijection.
    let k1 = mix(k0 ^ seed.rotate_left(32)) | 1;
    [k0 as usize, k1 as usize]
}

/// The mask a link stored at `at` is xored with: `(at ^ k0) * k1`, a different secret for every
/// block. Seeing one block's encoded null (the mask itself) reveals neither key.
///
/// The mask depends only on where the link is stored, so decoding computes it alongside the load
/// and adds a single xor to the allocation's dependency chain. (mimalloc's `rotl(p ^ k1, k0) + k0`
/// adds three dependent operations.)
#[inline(always)]
const fn mask(at: usize, keys: [usize; 2]) -> usize {
    (at ^ keys[0]).wrapping_mul(keys[1])
}

/// Stores `next` as `block`'s link.
///
/// # Safety
/// `block` is a block of `page` the caller may write.
#[inline(always)]
pub(crate) unsafe fn set_next(page: *const Page, block: *mut Block, next: *mut Block) {
    // SAFETY: `keys` is immutable while the page has blocks; the block is the caller's.
    unsafe { (*block).next = next.addr() ^ mask(block.addr(), (*page).keys) }
}

/// The block after `block` in its list.
///
/// # Safety
/// The calling thread owns `page`, and `block` is in one of its free lists.
#[inline(always)]
pub(crate) unsafe fn next_of(page: *const Page, block: *mut Block) -> *mut Block {
    // SAFETY: a free block of a page the caller owns.
    let next = unsafe { (*block).next ^ mask(block.addr(), (*page).keys) };
    #[cfg(debug_assertions)]
    // SAFETY: as above.
    unsafe {
        check_link(page, next)
    };
    // Blocks share their segment's provenance.
    block.with_addr(next)
}

/// Aborts unless `next` is null or a block carved from `page`.
///
/// # Safety
/// The calling thread owns `page`.
#[cfg(debug_assertions)]
#[inline]
unsafe fn check_link(page: *const Page, next: usize) {
    // SAFETY: owner-side fields of a page the caller owns.
    let (area, size, capacity) = unsafe {
        (
            (*page).area.addr(),
            (*page).block_size,
            (*page).capacity as usize,
        )
    };
    let carved = next >= area && next - area < capacity * size;
    if next != 0 && !(carved && (next - area).is_multiple_of(size)) {
        corrupted(page.addr(), next);
    }
}

#[cfg(debug_assertions)]
#[cold]
#[inline(never)]
fn corrupted(page: usize, link: usize) -> ! {
    use core::fmt::Write;
    let mut out = marlin_pal::debug::OutputBuffer::new();
    let _ = writeln!(
        out,
        "marlin-mem: heap corruption: a free list of page {page:#x} links to {link:#x} \
         (a write to freed memory, or a double free)"
    );
    out.flush();
    marlin_pal::process::abort()
}

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
    COMMITTED_BYTES.fetch_add(header_commit(), Ordering::Relaxed);
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

/// Releases every cached segment to the OS.
pub(crate) fn release_cache() {
    while let Some(base) = ptr::NonNull::new(cache_pop()) {
        RESERVED_BYTES.fetch_sub(SEGMENT_SIZE, Ordering::Relaxed);
        COMMITTED_BYTES.fetch_sub(header_commit(), Ordering::Relaxed);
        // SAFETY: a cached segment is a whole reservation nobody uses.
        let _ = unsafe { vmem::release(base, SEGMENT_SIZE) };
    }
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
        (*seg).committed = header_commit();
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
        let mut committed = (*seg).committed;
        if size == SEGMENT_SIZE {
            // Decommit everything but the header; on reuse pages are committed again as needed,
            // and huge allocations get zeroed memory for free.
            let commit = header_commit();
            let tail = ptr::NonNull::new_unchecked(base.add(commit));
            if vmem::decommit(tail, SEGMENT_SIZE - commit).is_ok() {
                COMMITTED_BYTES.fetch_sub(committed - commit, Ordering::Relaxed);
                committed = commit;
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
        COMMITTED_BYTES.fetch_sub(committed, Ordering::Relaxed);
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
        COMMITTED_BYTES.fetch_add(committed - (*seg).committed, Ordering::Relaxed);
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
            COMMITTED_BYTES.fetch_add(end - committed, Ordering::Relaxed);
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
            let (_, len) = page_tail(seg, index);
            (*seg).committed += len;
            COMMITTED_BYTES.fetch_add(len, Ordering::Relaxed);
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
        (*page).keys = page_keys(page);
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

/// The part of page `index` committed and decommitted on its own: the whole page, less the
/// header's OS pages for page 0. Returns its offset in the segment and its length.
///
/// # Safety
/// `seg` is a live small or medium segment.
unsafe fn page_tail(seg: *mut Segment, index: usize) -> (usize, usize) {
    // SAFETY: `page_shift` is immutable for the segment's life.
    let shift = unsafe { (*seg).page_shift };
    let start = if index == 0 {
        header_commit()
    } else {
        index << shift
    };
    (start, ((index + 1) << shift) - start)
}

/// Returns an unused page's memory to the OS; `page_init` commits it again on reuse.
///
/// # Safety
/// The calling thread owns `seg`; page `index` is unused, committed and in no list.
pub(crate) unsafe fn page_decommit(seg: *mut Segment, index: usize) {
    // SAFETY: the page is unused, so nothing reads its memory.
    unsafe {
        let page = (&raw mut (*seg).pages).cast::<Page>().add(index);
        let (start, len) = page_tail(seg, index);
        let start = ptr::NonNull::new_unchecked(seg.cast::<u8>().add(start));
        if vmem::decommit(start, len).is_ok() {
            (*page).committed = false;
            (*seg).committed -= len;
            COMMITTED_BYTES.fetch_sub(len, Ordering::Relaxed);
        }
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
            set_next(page, block.cast(), next.cast());
            block = next;
        }
        set_next(page, block.cast(), null_mut());
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
            loop {
                let next = next_of(page, foreign);
                if next.is_null() {
                    break;
                }
                foreign = next;
                count += 1;
            }
            set_next(page, foreign, (*page).local_free);
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
        (*page).free = next_of(page, block);
        (*page).used += 1;
        block.cast()
    }
}
