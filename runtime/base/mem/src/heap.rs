//! Thread-local heaps: the allocation and free paths.
//!
//! Each thread allocates from its own [`Heap`] without atomics. A free of a block owned by the
//! calling thread's heap is just as cheap; any other free pushes the block onto its page's
//! `thread_free` stack, which the owner collects when it runs out of blocks.
//!
//! When a thread exits, a TLS destructor *abandons* its heap: empty segments go back to the
//! cache, the rest wait in a global list until another heap needing a segment *reclaims* them.
//! Threads that allocate before their heap exists or after it is gone use a global heap under a
//! lock.
//!
//! # Purging
//!
//! A page whose blocks have all been freed is *retired*: it goes back to its segment, unused but
//! still committed, and joins its heap's retired queue. Pages retired longer than the
//! [purge delay](set_purge_delay) ago are decommitted the next time the owning thread retires a
//! page or needs a new one, so memory freed in a burst returns to the OS shortly after, while a
//! page freed and needed again within the delay costs no system call. [`purge`] does it at once.

use core::cell::Cell;
use core::ffi::c_void;
use core::ptr::{self, null_mut};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use core::time::Duration;

use marlin_pal::tls::Key;
use marlin_pal::{Protection, time, vmem};

use crate::layout::{BIN_COUNT, MEDIUM_MAX, SMALL_MAX, bin, bin_size};
use crate::list::intrusive_list;
use crate::lock::Locked;
use crate::segment::{
    Block, Kind, Page, Segment, block_start, huge_alloc, huge_grow, page_collect, page_decommit,
    page_extend, page_init, page_of, page_pop, segment_alloc, segment_of, segment_release,
    set_next, usable_size,
};

intrusive_list!(PageQueue<Page> via next, prev);
intrusive_list!(SegmentQueue<Segment> via free_next, free_prev);
intrusive_list!(SegmentList<Segment> via owned_next, owned_prev);

#[repr(C, align(64))]
pub(crate) struct Heap {
    /// Per bin: pages that may have free blocks. The first one serves the fast path.
    queues: [PageQueue; BIN_COUNT],
    /// Per bin: pages without free blocks.
    full: [PageQueue; BIN_COUNT],
    /// Per bin: set by another thread after freeing into a page in `full`.
    full_hint: [AtomicBool; BIN_COUNT],
    /// Small and medium segments with unused pages.
    free_segments: [SegmentQueue; 2],
    owned: SegmentList,
    /// Unused, committed pages of owned segments, oldest first.
    retired: PageQueue,
    pool_next: *mut Heap,
}

const UNINIT: usize = 0;
const EXITED: usize = 1;

/// The calling thread's heap, or `UNINIT` / `EXITED`.
#[thread_local]
static THREAD_HEAP: Cell<usize> = Cell::new(UNINIT);

/// Reads [`THREAD_HEAP`] afresh on every call.
///
/// Fibers migrate between threads, and LLVM may reuse a thread-local's address across an opaque
/// call within one function. Inlined into a job that allocates, waits (resuming on another thread)
/// and allocates again, a direct read could hand it the old thread's heap.
#[inline(never)]
fn thread_heap() -> usize {
    THREAD_HEAP.get()
}

#[derive(Clone, Copy)]
enum KeyState {
    Uninit,
    Ready(Key),
    Failed,
}

/// Registers `thread_exit` for every thread with a heap.
static KEY: Locked<KeyState> = Locked::new(KeyState::Uninit);
/// For threads without a heap of their own.
static GLOBAL_HEAP: Locked<*mut Heap> = Locked::new(null_mut());
static ABANDONED: Locked<SegmentList> = Locked::new(SegmentList::new());
static ABANDONED_COUNT: AtomicUsize = AtomicUsize::new(0);

// --- Allocation ---

/// # Safety
/// Same contract as `GlobalAlloc::alloc`, except `size` may be 0.
#[inline]
pub(crate) unsafe fn malloc(size: usize, align: usize) -> *mut u8 {
    if align <= 16 && size <= MEDIUM_MAX {
        // Every bin of a multiple of 16 is 16-aligned.
        let size = if align > 8 {
            size.max(1).next_multiple_of(16)
        } else {
            size.max(1)
        };
        // SAFETY: `size` is within the bins.
        unsafe { alloc_in(bin(size)) }
    } else {
        // SAFETY: forwarded from the caller.
        unsafe { malloc_large_or_aligned(size, align) }
    }
}

#[cold]
unsafe fn malloc_large_or_aligned(size: usize, align: usize) -> *mut u8 {
    // Over-aligned and small: over-allocate in a page and hand out an aligned interior pointer;
    // the page is flagged so `free` rounds back to the block.
    if align > 16
        && let Some(padded) = size.max(1).checked_add(align - 1)
        && padded <= MEDIUM_MAX
    {
        // SAFETY: `padded` is within the bins; the block is ours, so is its page.
        unsafe {
            let p = alloc_in(bin(padded));
            if p.is_null() {
                return p;
            }
            let aligned = p.map_addr(|a| a.next_multiple_of(align));
            if aligned != p {
                (*page_of(segment_of(p), p))
                    .has_aligned
                    .store(true, Ordering::Relaxed);
            }
            return aligned;
        }
    }
    huge_alloc(size, align)
}

/// # Safety
/// Same contract as [`malloc`].
pub(crate) unsafe fn malloc_zeroed(size: usize, align: usize) -> *mut u8 {
    // SAFETY: forwarded from the caller; the block is valid for `size` bytes.
    unsafe {
        let p = malloc(size, align);
        // Huge allocations come straight from freshly committed pages.
        if !p.is_null() && (*segment_of(p)).kind != Kind::Huge {
            ptr::write_bytes(p, 0, size);
        }
        p
    }
}

/// # Safety
/// Same contract as `GlobalAlloc::realloc`.
pub(crate) unsafe fn realloc(
    p: *mut u8,
    old_size: usize,
    align: usize,
    new_size: usize,
) -> *mut u8 {
    // SAFETY: forwarded from the caller.
    unsafe {
        let usable = usable_size(p);
        // Fits, and does not waste more than half the block.
        if new_size <= usable && new_size >= usable / 2 {
            return p;
        }
        if new_size > usable && huge_grow(p, new_size) {
            return p;
        }
        let q = malloc(new_size, align);
        if !q.is_null() {
            ptr::copy_nonoverlapping(p, q, old_size.min(new_size));
            free(p);
        }
        q
    }
}

/// # Safety
/// `bin` is in `1..BIN_COUNT`.
#[inline]
unsafe fn alloc_in(bin: usize) -> *mut u8 {
    let state = thread_heap();
    // SAFETY: a thread heap is only used by its thread.
    unsafe {
        if state > EXITED {
            alloc_bin(state as *mut Heap, bin)
        } else {
            alloc_without_heap(state, bin)
        }
    }
}

#[cold]
unsafe fn alloc_without_heap(state: usize, bin: usize) -> *mut u8 {
    // SAFETY: a fresh thread heap belongs to this thread; the global heap is used under its lock.
    unsafe {
        if state == UNINIT {
            let heap = init_thread_heap();
            if !heap.is_null() {
                return alloc_bin(heap, bin);
            }
        }
        GLOBAL_HEAP.with(|heap| {
            if heap.is_null() {
                *heap = pool_alloc();
                if heap.is_null() {
                    return null_mut();
                }
            }
            alloc_bin(*heap, bin)
        })
    }
}

/// # Safety
/// The calling thread has exclusive use of `heap`.
#[inline]
unsafe fn alloc_bin(heap: *mut Heap, bin: usize) -> *mut u8 {
    // SAFETY: the heap's pages are owned by the calling thread.
    unsafe {
        let page = (*heap).queues[bin].first;
        if !page.is_null() && !(*page).free.is_null() {
            return page_pop(page);
        }
        alloc_slow(heap, bin)
    }
}

#[cold]
unsafe fn alloc_slow(heap: *mut Heap, bin: usize) -> *mut u8 {
    // SAFETY: the heap and its pages are owned by the calling thread.
    unsafe {
        let queue = &raw mut (*heap).queues[bin];

        // Collect frees and carve fresh blocks; move pages that are still full out of the way.
        let mut page = (*queue).first;
        while !page.is_null() {
            let next = (*page).next;
            page_collect(page);
            if (*page).free.is_null() && (*page).capacity < (*page).reserved {
                page_extend(page);
            }
            if !(*page).free.is_null() || !move_to_full(heap, page) {
                (*queue).move_to_front(page);
                return page_pop(page);
            }
            page = next;
        }

        // Full pages that other threads freed into since the last look.
        if (*heap).full_hint[bin].swap(false, Ordering::Acquire) {
            let full = &raw mut (*heap).full[bin];
            let mut page = (*full).first;
            while !page.is_null() {
                let next = (*page).next;
                if !(*page).thread_free.load(Ordering::Relaxed).is_null() {
                    page_collect(page);
                    (*page).in_full.store(false, Ordering::Relaxed);
                    (*full).remove(page);
                    (*queue).push_front(page);
                }
                page = next;
            }
            let first = (*queue).first;
            if !first.is_null() && !(*first).free.is_null() {
                return page_pop(first);
            }
        }

        let page = new_page(heap, bin);
        if page.is_null() {
            return null_mut();
        }
        (*queue).push_front(page);
        page_extend(page);
        page_pop(page)
    }
}

/// Moves a page without free blocks to the full queue. Returns false, leaving it in place with
/// blocks, if a foreign free raced in.
///
/// # Safety
/// The calling thread owns `heap`, and `page` is in its queue with an empty `free` list.
unsafe fn move_to_full(heap: *mut Heap, page: *mut Page) -> bool {
    // SAFETY: the page and heap are owned by the calling thread.
    unsafe {
        // Pairs with `free_foreign`: with both sides SeqCst, either we see its block or it sees
        // `in_full` and leaves a hint.
        (*page).in_full.store(true, Ordering::SeqCst);
        if !(*page).thread_free.load(Ordering::SeqCst).is_null() {
            (*page).in_full.store(false, Ordering::Relaxed);
            page_collect(page);
            return false;
        }
        let bin = (*page).bin as usize;
        (*heap).queues[bin].remove(page);
        (*heap).full[bin].push_front(page);
        true
    }
}

/// A fresh page for `bin`, or null when out of memory.
///
/// # Safety
/// The calling thread owns `heap`.
unsafe fn new_page(heap: *mut Heap, bin: usize) -> *mut Page {
    let kind = if bin_size(bin) <= SMALL_MAX {
        Kind::Small
    } else {
        Kind::Medium
    };
    // SAFETY: the heap and its segments are owned by the calling thread.
    unsafe {
        let segments = &raw mut (*heap).free_segments[kind as usize];
        // Abandoned segments whose blocks were all freed by other threads are only released
        // here, so visit a few even when the first is empty.
        for _ in 0..8 {
            if !(*segments).first.is_null() || !reclaim_abandoned(heap, kind) {
                break;
            }
        }
        let mut seg = (*segments).first;
        if seg.is_null() {
            seg = segment_alloc(kind);
            if seg.is_null() {
                return null_mut();
            }
            (*seg).heap.store(heap, Ordering::Relaxed);
            (*heap).owned.push_front(seg);
            (*segments).push_front(seg);
            (*seg).in_free_queue = true;
        }

        // A retired page saves committing one.
        let unused = |i: &usize| !(*seg).pages[*i].in_use;
        let index = (0..(*seg).page_count)
            .find(|i| unused(i) && (*seg).pages[*i].retired)
            .or_else(|| (0..(*seg).page_count).find(unused))
            .expect("segment in the free queue has no unused page");
        let page = &raw mut (*seg).pages[index];
        if (*page).retired {
            (*page).retired = false;
            (*heap).retired.remove(page);
        }
        if !(*heap).retired.first.is_null() {
            purge_expired(heap, time::ticks());
        }
        let page = page_init(seg, index, bin);
        if page.is_null() {
            return null_mut();
        }
        (*seg).used_pages += 1;
        if (*seg).used_pages == (*seg).page_count {
            (*segments).remove(seg);
            (*seg).in_free_queue = false;
        }
        page
    }
}

// --- Free ---

/// # Safety
/// `p` is null or a live allocation from this allocator.
#[inline]
pub(crate) unsafe fn free(p: *mut u8) {
    if p.is_null() {
        return;
    }
    let seg = segment_of(p);
    // SAFETY: `p` is live, so its segment and page are too.
    unsafe {
        if (*seg).kind == Kind::Huge {
            return segment_release(seg);
        }
        let page = page_of(seg, p);
        let block = if (*page).has_aligned.load(Ordering::Relaxed) {
            block_start(page, p)
        } else {
            p.cast::<Block>()
        };
        let state = thread_heap();
        if state > EXITED && (*seg).heap.load(Ordering::Relaxed).addr() == state {
            free_local(state as *mut Heap, page, block);
        } else {
            free_foreign(seg, page, block);
        }
    }
}

/// # Safety
/// The calling thread owns `heap`, which owns `page`; `block` is a live block of `page`.
#[inline]
unsafe fn free_local(heap: *mut Heap, page: *mut Page, block: *mut Block) {
    // SAFETY: owner-side page fields belong to the calling thread.
    unsafe {
        set_next(page, block, (*page).local_free);
        (*page).local_free = block;
        debug_assert!((*page).used > 0, "double free");
        (*page).used -= 1;
        if (*page).used == 0 || (*page).in_full.load(Ordering::Relaxed) {
            free_local_slow(heap, page);
        }
    }
}

#[cold]
unsafe fn free_local_slow(heap: *mut Heap, page: *mut Page) {
    // SAFETY: as in `free_local`.
    unsafe {
        let bin = (*page).bin as usize;
        let queue = &raw mut (*heap).queues[bin];
        if (*page).in_full.load(Ordering::Relaxed) {
            (*page).in_full.store(false, Ordering::Relaxed);
            (*heap).full[bin].remove(page);
            (*queue).push_front(page);
        }
        // Keep the bin's last page to avoid churn; retire any other empty one.
        if (*page).used == 0 && !((*queue).first == page && (*page).next.is_null()) {
            retire_page(heap, page);
        }
    }
}

/// # Safety
/// `block` is a live block of `page` in `seg`, owned by another heap (or none).
unsafe fn free_foreign(seg: *mut Segment, page: *mut Page, block: *mut Block) {
    // SAFETY: the block is ours to write; the rest are atomics or immutable while it is live.
    unsafe {
        let thread_free = &(*page).thread_free;
        let mut head = thread_free.load(Ordering::Relaxed);
        loop {
            set_next(page, block, head);
            match thread_free.compare_exchange_weak(
                head,
                block,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => head = current,
            }
        }
        // See `move_to_full`.
        if (*page).in_full.load(Ordering::SeqCst) {
            // Heaps are never unmapped, so a stale owner only gets a spurious hint.
            let heap = (*seg).heap.load(Ordering::Acquire);
            if !heap.is_null() {
                (*heap).full_hint[(*page).bin as usize].store(true, Ordering::Release);
            }
        }
    }
}

/// Returns an empty page to its segment, and the segment to the cache once it is empty.
///
/// # Safety
/// The calling thread owns `heap`; `page` is in its queue and has no live blocks.
unsafe fn retire_page(heap: *mut Heap, page: *mut Page) {
    // SAFETY: the heap, page and segment are owned by the calling thread.
    unsafe {
        (*heap).queues[(*page).bin as usize].remove(page);
        (*page).in_use = false;
        let seg = segment_of(page);
        (*seg).used_pages -= 1;
        let segments = &raw mut (*heap).free_segments[(*seg).kind as usize];
        if (*seg).used_pages == 0 {
            if (*seg).in_free_queue {
                (*segments).remove(seg);
            }
            forget_retired(heap, seg);
            (*heap).owned.remove(seg);
            (*seg).heap.store(null_mut(), Ordering::Relaxed);
            segment_release(seg);
        } else {
            if !(*seg).in_free_queue {
                (*segments).push_front(seg);
                (*seg).in_free_queue = true;
            }
            let now = time::ticks();
            (*page).retired = true;
            (*page).retired_at = now;
            (*heap).retired.push_back(page);
            purge_expired(heap, now);
        }
    }
}

/// Takes the retired pages of `seg`, which is about to be released, off the retired queue.
///
/// # Safety
/// The calling thread owns `heap`, which owns `seg`.
unsafe fn forget_retired(heap: *mut Heap, seg: *mut Segment) {
    // SAFETY: the segment's pages are owned by the calling thread.
    unsafe {
        for i in 0..(*seg).page_count {
            let page = &raw mut (*seg).pages[i];
            if (*page).retired {
                (*page).retired = false;
                (*heap).retired.remove(page);
            }
        }
    }
}

/// Decommits the unused pages of `seg` still holding memory, when no heap's retired queue holds
/// them: `seg` is being abandoned, or was until now.
///
/// # Safety
/// The calling thread has exclusive use of `seg`, and no retired queue holds its pages.
unsafe fn decommit_unused(seg: *mut Segment) {
    // SAFETY: as above.
    unsafe {
        for i in 0..(*seg).page_count {
            let page = &raw mut (*seg).pages[i];
            (*page).retired = false;
            if !(*page).in_use && (*page).committed {
                page_decommit(seg, i);
            }
        }
    }
}

// --- Purge ---

/// The delay in ticks, or one of the two values below.
static PURGE_DELAY: AtomicU64 = AtomicU64::new(DELAY_DEFAULT);
const DELAY_NEVER: u64 = u64::MAX;
/// [`DEFAULT_PURGE_DELAY`], not yet converted to ticks.
const DELAY_DEFAULT: u64 = u64::MAX - 1;

/// How long a retired page keeps its memory by default.
pub const DEFAULT_PURGE_DELAY: Duration = Duration::from_millis(100);

fn to_ticks(delay: Duration) -> u64 {
    let ticks = delay.as_nanos() * time::ticks_per_second() as u128 / 1_000_000_000;
    ticks.min(DELAY_DEFAULT as u128 - 1) as u64
}

fn purge_delay_ticks() -> u64 {
    match PURGE_DELAY.load(Ordering::Relaxed) {
        DELAY_DEFAULT => {
            let ticks = to_ticks(DEFAULT_PURGE_DELAY);
            let _ = PURGE_DELAY.compare_exchange(
                DELAY_DEFAULT,
                ticks,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
            ticks
        }
        delay => delay,
    }
}

/// Sets how long the memory of a page whose blocks were all freed stays committed, waiting to be
/// reused, before it is returned to the OS: [`DEFAULT_PURGE_DELAY`] unless changed. `Some(ZERO)`
/// decommits pages as soon as they empty; `None` never purges on its own, leaving it to
/// [`purge`].
///
/// Expired pages are purged by the thread that owns them, the next time it empties a page or
/// needs a new one; a thread that stops allocating keeps its pages until then.
pub fn set_purge_delay(delay: Option<Duration>) {
    PURGE_DELAY.store(delay.map_or(DELAY_NEVER, to_ticks), Ordering::Relaxed);
}

/// The current [purge delay](set_purge_delay), rounded to clock ticks.
pub fn purge_delay() -> Option<Duration> {
    match purge_delay_ticks() {
        DELAY_NEVER => None,
        ticks => {
            let nanos = ticks as u128 * 1_000_000_000 / time::ticks_per_second() as u128;
            Some(Duration::from_nanos(nanos as u64))
        }
    }
}

/// Decommits the pages retired at least the purge delay before `now`.
///
/// # Safety
/// The calling thread owns `heap`.
unsafe fn purge_expired(heap: *mut Heap, now: u64) {
    let delay = purge_delay_ticks();
    if delay == DELAY_NEVER {
        return;
    }
    // SAFETY: the heap and its retired pages are owned by the calling thread.
    unsafe {
        loop {
            let page = (*heap).retired.first;
            if page.is_null() || now.wrapping_sub((*page).retired_at) < delay {
                break;
            }
            purge_page(heap, page);
        }
    }
}

/// # Safety
/// The calling thread owns `heap`, and `page` is in its retired queue.
unsafe fn purge_page(heap: *mut Heap, page: *mut Page) {
    // SAFETY: the page is unused and owned by the calling thread.
    unsafe {
        (*heap).retired.remove(page);
        (*page).retired = false;
        let seg = segment_of(page);
        let index = (page.addr() - (&raw const (*seg).pages).addr()) / size_of::<Page>();
        page_decommit(seg, index);
    }
}

/// # Safety
/// The calling thread owns `heap`.
unsafe fn purge_all(heap: *mut Heap) {
    // SAFETY: as above.
    unsafe {
        while !(*heap).retired.first.is_null() {
            purge_page(heap, (*heap).retired.first);
        }
    }
}

/// Returns memory to the OS now: decommits the pages the calling thread's heap has retired, and
/// releases the cached empty segments.
///
/// Other threads' retired pages are purged by those threads (see [`set_purge_delay`]); abandoned
/// segments by [`collect_abandoned`].
pub fn purge() {
    let state = thread_heap();
    // SAFETY: a thread heap is only used by its thread; the global heap under its lock.
    unsafe {
        if state > EXITED {
            purge_all(state as *mut Heap);
        }
        GLOBAL_HEAP.with(|heap| {
            if !heap.is_null() {
                purge_all(*heap);
            }
        });
    }
    crate::segment::release_cache();
}

// --- Thread lifecycle ---

#[cold]
unsafe fn init_thread_heap() -> *mut Heap {
    let heap = pool_alloc();
    if heap.is_null() {
        return heap;
    }
    let key = KEY.with(|state| {
        if let KeyState::Uninit = state {
            *state = Key::new(Some(thread_exit)).map_or(KeyState::Failed, KeyState::Ready);
        }
        match *state {
            KeyState::Ready(key) => Some(key),
            _ => None,
        }
    });
    // Without a key the heap leaks at thread exit, but allocation still works.
    if let Some(key) = key {
        // SAFETY: the key is never deleted.
        let _ = unsafe { key.set(heap.cast()) };
    }
    THREAD_HEAP.set(heap.addr());
    heap
}

unsafe extern "C" fn thread_exit(heap: *mut c_void) {
    // Later frees on this thread take the foreign path; later allocations use the global heap.
    THREAD_HEAP.set(EXITED);
    // SAFETY: the heap belonged to this thread, which no longer uses it.
    unsafe {
        abandon(heap.cast());
        pool_free(heap.cast());
    }
}

/// Gives up every segment of `heap`: empty ones are released, the rest wait for reclaiming.
///
/// # Safety
/// The calling thread owns `heap` and never uses it again.
unsafe fn abandon(heap: *mut Heap) {
    // SAFETY: the heap's segments are owned by the calling thread until handed to ABANDONED.
    unsafe {
        let mut seg = (*heap).owned.first;
        while !seg.is_null() {
            let next = (*seg).owned_next;
            for i in 0..(*seg).page_count {
                let page = &raw mut (*seg).pages[i];
                if !(*page).in_use {
                    continue;
                }
                page_collect(page);
                if (*page).used == 0 {
                    (*page).in_use = false;
                    (*seg).used_pages -= 1;
                } else {
                    (*page).in_full.store(false, Ordering::Relaxed);
                    (*page).next = null_mut();
                    (*page).prev = null_mut();
                }
            }
            (*seg).in_free_queue = false;
            (*seg).free_next = null_mut();
            (*seg).free_prev = null_mut();
            (*seg).heap.store(null_mut(), Ordering::Release);
            if (*seg).used_pages == 0 {
                segment_release(seg);
            } else {
                // Nobody would purge them while abandoned.
                decommit_unused(seg);
                ABANDONED.with(|list| list.push_front(seg));
                ABANDONED_COUNT.fetch_add(1, Ordering::Relaxed);
            }
            seg = next;
        }
    }
}

/// Takes one abandoned segment of `kind`: releases it if it has become empty, otherwise adopts it
/// into `heap`. Returns false if there was none.
///
/// # Safety
/// The calling thread owns `heap`.
unsafe fn reclaim_abandoned(heap: *mut Heap, kind: Kind) -> bool {
    // SAFETY: a segment taken off ABANDONED (whose lock orders it after the abandoning thread's
    // writes) is owned by nobody until we adopt it.
    unsafe {
        let seg = ABANDONED.with(|list| {
            let mut seg = list.first;
            while !seg.is_null() && (*seg).kind != kind {
                seg = (*seg).owned_next;
            }
            if !seg.is_null() {
                list.remove(seg);
            }
            seg
        });
        if seg.is_null() {
            return false;
        }
        ABANDONED_COUNT.fetch_sub(1, Ordering::Relaxed);

        (*seg).heap.store(heap, Ordering::Release);
        (*heap).owned.push_front(seg);
        for i in 0..(*seg).page_count {
            let page = &raw mut (*seg).pages[i];
            if !(*page).in_use {
                continue;
            }
            page_collect(page);
            if (*page).used == 0 {
                (*page).in_use = false;
                (*seg).used_pages -= 1;
            } else {
                (*heap).queues[(*page).bin as usize].push_back(page);
            }
        }
        if (*seg).used_pages == 0 {
            (*heap).owned.remove(seg);
            (*seg).heap.store(null_mut(), Ordering::Relaxed);
            segment_release(seg);
        } else {
            decommit_unused(seg);
            if (*seg).used_pages < (*seg).page_count {
                (*heap).free_segments[kind as usize].push_front(seg);
                (*seg).in_free_queue = true;
            }
        }
        true
    }
}

/// Releases abandoned segments whose blocks have all been freed by other threads.
///
/// Abandoned segments are otherwise only revisited when some thread needs a new segment. Call
/// this after freeing a lot of memory that exited threads allocated, e.g. on level unload.
pub fn collect_abandoned() {
    let mut list = ABANDONED.with(|list| core::mem::replace(list, SegmentList::new()));
    // SAFETY: segments taken off ABANDONED are owned by nobody else until they are put back.
    unsafe {
        let mut seg = list.first;
        while !seg.is_null() {
            let next = (*seg).owned_next;
            for i in 0..(*seg).page_count {
                let page = &raw mut (*seg).pages[i];
                if (*page).in_use {
                    page_collect(page);
                    if (*page).used == 0 {
                        (*page).in_use = false;
                        (*seg).used_pages -= 1;
                    }
                }
            }
            list.remove(seg);
            if (*seg).used_pages == 0 {
                ABANDONED_COUNT.fetch_sub(1, Ordering::Relaxed);
                segment_release(seg);
            } else {
                decommit_unused(seg);
                ABANDONED.with(|abandoned| abandoned.push_front(seg));
            }
            seg = next;
        }
    }
}

// --- Heap pool ---

/// Heaps are carved from OS chunks and recycled, never unmapped: a thread racing with an
/// abandon may still write a hint into its old heap.
struct Pool {
    free: *mut Heap,
    cursor: *mut u8,
    end: *mut u8,
}

const POOL_CHUNK: usize = 64 << 10;

static POOL: Locked<Pool> = Locked::new(Pool {
    free: null_mut(),
    cursor: null_mut(),
    end: null_mut(),
});

fn pool_alloc() -> *mut Heap {
    POOL.with(|pool| {
        let heap = if !pool.free.is_null() {
            let heap = pool.free;
            // SAFETY: pooled heaps are valid and unused.
            pool.free = unsafe { (*heap).pool_next };
            heap
        } else {
            let size = size_of::<Heap>();
            if pool.cursor.is_null() || pool.end.addr() - pool.cursor.addr() < size {
                let Ok(chunk) = vmem::reserve(POOL_CHUNK) else {
                    return null_mut();
                };
                // SAFETY: commits the reservation just made.
                if unsafe { vmem::commit(chunk, POOL_CHUNK, Protection::ReadWrite) }.is_err() {
                    // SAFETY: releases the reservation just made.
                    let _ = unsafe { vmem::release(chunk, POOL_CHUNK) };
                    return null_mut();
                }
                pool.cursor = chunk.as_ptr();
                // SAFETY: one past the end of the chunk.
                pool.end = unsafe { pool.cursor.add(POOL_CHUNK) };
            }
            let heap = pool.cursor.cast::<Heap>();
            // SAFETY: the chunk has room for one more heap; chunks are page aligned and heaps are
            // a multiple of their alignment in size.
            pool.cursor = unsafe { pool.cursor.add(size) };
            heap
        };
        // SAFETY: the heap is unused. `full_hint` is reset atomically since a stale foreign free
        // may still write to it (see `free_foreign`).
        unsafe {
            (&raw mut (*heap).queues).write([const { PageQueue::new() }; BIN_COUNT]);
            (&raw mut (*heap).full).write([const { PageQueue::new() }; BIN_COUNT]);
            for hint in &(*heap).full_hint {
                hint.store(false, Ordering::Relaxed);
            }
            (&raw mut (*heap).free_segments).write([const { SegmentQueue::new() }; 2]);
            (&raw mut (*heap).owned).write(SegmentList::new());
            (&raw mut (*heap).retired).write(PageQueue::new());
            (&raw mut (*heap).pool_next).write(null_mut());
        }
        heap
    })
}

/// # Safety
/// `heap` came from `pool_alloc` and is no longer used.
unsafe fn pool_free(heap: *mut Heap) {
    POOL.with(|pool| {
        // SAFETY: the heap is ours to recycle.
        unsafe { (*heap).pool_next = pool.free };
        pool.free = heap;
    });
}

// --- Statistics ---

/// A snapshot of the allocator's OS-level state.
#[derive(Clone, Copy, Debug)]
pub struct HeapStats {
    /// Address space reserved for segments, including cached ones.
    pub reserved_bytes: usize,
    /// Memory committed in segments: headers, pages in use, retired pages not yet purged, and
    /// huge allocations. On Linux, overcommit means some of it may not be backed yet.
    pub committed_bytes: usize,
    /// Empty segments kept for reuse (only their headers are committed).
    pub cached_segments: usize,
    /// Segments of exited threads still holding live blocks, waiting to be reclaimed.
    pub abandoned_segments: usize,
}

pub fn stats() -> HeapStats {
    HeapStats {
        reserved_bytes: crate::segment::RESERVED_BYTES.load(Ordering::Relaxed),
        committed_bytes: crate::segment::COMMITTED_BYTES.load(Ordering::Relaxed),
        cached_segments: crate::segment::cached_segments(),
        abandoned_segments: ABANDONED_COUNT.load(Ordering::Relaxed),
    }
}

/// The usable size of an allocation of `size` bytes (alignment ≤ 16): the size class a small or
/// medium request is rounded up to, or whole pages for larger ones. Containers can grow to it for
/// free.
pub fn good_size(size: usize) -> usize {
    if size <= MEDIUM_MAX {
        bin_size(bin(size.max(1)))
    } else {
        size.next_multiple_of(vmem::page_size())
    }
}
