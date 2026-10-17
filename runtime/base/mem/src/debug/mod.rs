//! Allocation debugging for [`MarlinHeap`](crate::MarlinHeap) (feature `mem-debug`).
//!
//! With the feature on, every heap allocation and free, through `GlobalAlloc` or `Allocator`,
//! passes through this layer:
//!
//! - **Leak tracking.** Each live allocation is recorded in a table: its address, its size and,
//!   if a [capture hook](set_capture_hook) is installed, up to [`MAX_FRAMES`] return addresses.
//!   [`for_each_live`], [`live_count`] and [`live_bytes`] read it, e.g. to report leaks at
//!   shutdown. A free of a pointer that is not live (a double free, a foreign pointer) or with
//!   the wrong size aborts with a message.
//! - **Guard pages**, once [enabled](set_guard_pages). Each new allocation gets committed pages of
//!   its own, placed so it ends where an inaccessible page begins (as closely as its alignment
//!   allows): an overflow faults on the spot. A freed block's pages are decommitted, so a use
//!   after free faults too, and stay reserved in a quarantine of [`QUARANTINE`] blocks before
//!   their address space is released. Costs two system calls and at least one page (64 KiB of
//!   address space on Windows) per allocation; reallocation always moves.
//!
//! The table and the guard blocks take memory straight from the OS, never from the heap, so this
//! layer never allocates through itself. Everything goes through one lock: slow, for debugging.

mod guard;
mod table;

use core::cell::Cell;
use core::fmt::Write;
use core::ptr::{self, NonNull, null_mut};
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

pub use guard::QUARANTINE;
use table::{Entry, InsertError, TABLE};

use crate::heap;

/// Frames recorded per allocation, at most.
pub const MAX_FRAMES: usize = 16;

/// Captures the calling stack: writes return addresses into the slice, innermost first, and
/// returns how many it wrote. It may allocate (that allocation is recorded without frames).
pub type CaptureFn = fn(&mut [usize]) -> usize;

static HOOK: AtomicPtr<()> = AtomicPtr::new(null_mut());
static GUARD_PAGES: AtomicBool = AtomicBool::new(false);

/// Installs the stack-capture hook, or removes it with `None`. Allocations made while it is
/// installed carry their call stack.
pub fn set_capture_hook(hook: Option<CaptureFn>) {
    let hook = hook.map_or(null_mut(), |f| f as *mut ());
    HOOK.store(hook, Ordering::Release);
}

/// Turns guard pages on or off for allocations made from now on. Off by default. Blocks keep the
/// kind they were allocated as.
pub fn set_guard_pages(enabled: bool) {
    GUARD_PAGES.store(enabled, Ordering::Relaxed);
}

pub fn guard_pages() -> bool {
    GUARD_PAGES.load(Ordering::Relaxed)
}

/// Live heap allocations.
pub fn live_count() -> usize {
    TABLE.with(|t| t.len())
}

/// Bytes in live heap allocations, as requested.
pub fn live_bytes() -> usize {
    TABLE.with(|t| t.bytes())
}

/// Calls `f(address, size, frames)` for every live heap allocation, in no particular order.
///
/// `f` runs outside the table's lock and may allocate. The walk is not a snapshot: allocations
/// made or freed meanwhile may or may not be seen, and growth of the table during the walk may
/// show an allocation twice or not at all.
pub fn for_each_live(mut f: impl FnMut(usize, usize, &[usize])) {
    let mut batch = [Entry::EMPTY; 32];
    let mut cursor = 0;
    loop {
        let n = table::next_batch(&mut cursor, &mut batch);
        if n == 0 {
            break;
        }
        for entry in &batch[..n] {
            f(entry.addr, entry.size, entry.frames());
        }
    }
}

/// Set while the capture hook runs, so its own allocations do not capture again.
#[thread_local]
static CAPTURING: Cell<bool> = Cell::new(false);

/// Fills `entry`'s frames through the hook, if there is one.
///
/// Not inlined, like every thread-local access that may run on a fiber (see `heap::thread_heap`).
#[inline(never)]
fn capture(entry: &mut Entry) {
    let hook = HOOK.load(Ordering::Acquire);
    if hook.is_null() || CAPTURING.get() {
        return;
    }
    // SAFETY: only `set_capture_hook` stores to HOOK, and only `CaptureFn`s.
    let hook = unsafe { core::mem::transmute::<*mut (), CaptureFn>(hook) };
    CAPTURING.set(true);
    entry.frame_count = hook(&mut entry.frames).min(MAX_FRAMES);
    CAPTURING.set(false);
}

#[cold]
#[inline(never)]
fn fatal(what: &str, addr: usize, size: usize) -> ! {
    let mut out = marlin_pal::debug::OutputBuffer::new();
    let _ = writeln!(out, "marlin-mem: {what}: {addr:#x} ({size} bytes)");
    out.flush();
    marlin_pal::process::abort()
}

fn record(entry: &Entry) {
    match TABLE.with(|t| t.insert(entry)) {
        Ok(()) => {}
        Err(InsertError::Duplicate) => fatal(
            "heap corruption: block handed out twice",
            entry.addr,
            entry.size,
        ),
        Err(InsertError::OutOfMemory) => {
            fatal("out of memory for the leak table", entry.addr, entry.size)
        }
    }
}

/// # Safety
/// Same contract as `heap::malloc`.
unsafe fn alloc(size: usize, align: usize, zeroed: bool) -> *mut u8 {
    let mut entry = Entry::EMPTY;
    entry.size = size;
    capture(&mut entry);
    let p = if guard_pages() {
        match guard::alloc(size, align) {
            Some(block) => {
                entry.guard_base = block.base.as_ptr();
                entry.guard_len = block.len;
                // Fresh pages are zero.
                block.ptr.as_ptr()
            }
            None => null_mut(),
        }
    } else if zeroed {
        // SAFETY: forwarded from the caller.
        unsafe { heap::malloc_zeroed(size, align) }
    } else {
        // SAFETY: forwarded from the caller.
        unsafe { heap::malloc(size, align) }
    };
    if !p.is_null() {
        entry.addr = p.addr();
        record(&entry);
    }
    p
}

/// # Safety
/// Same contract as `heap::malloc`.
pub(crate) unsafe fn malloc(size: usize, align: usize) -> *mut u8 {
    // SAFETY: forwarded from the caller.
    unsafe { alloc(size, align, false) }
}

/// # Safety
/// Same contract as `heap::malloc`.
pub(crate) unsafe fn malloc_zeroed(size: usize, align: usize) -> *mut u8 {
    // SAFETY: forwarded from the caller.
    unsafe { alloc(size, align, true) }
}

/// Frees the block an entry, already out of the table, describes.
///
/// # Safety
/// The entry's block is live and no longer used.
unsafe fn release(p: *mut u8, entry: &Entry) {
    // SAFETY: forwarded from the caller.
    unsafe {
        match NonNull::new(entry.guard_base) {
            Some(base) => guard::free(base, entry.guard_len),
            None => heap::free(p),
        }
    }
}

/// # Safety
/// `p` is null or a live allocation of `size` bytes from this allocator.
pub(crate) unsafe fn free(p: *mut u8, size: usize) {
    if p.is_null() {
        return;
    }
    // Out of the table first: once the heap has the block back, another thread may get it.
    let Some(entry) = TABLE.with(|t| t.remove(p.addr())) else {
        fatal(
            "free of a pointer that is not a live allocation",
            p.addr(),
            size,
        )
    };
    if entry.size != size {
        fatal(
            "free with the wrong size (allocated with the size in parentheses)",
            p.addr(),
            entry.size,
        );
    }
    // SAFETY: a live block, now ours to free (the caller).
    unsafe { release(p, &entry) }
}

/// # Safety
/// Same contract as `heap::realloc`.
pub(crate) unsafe fn realloc(
    p: *mut u8,
    old_size: usize,
    align: usize,
    new_size: usize,
) -> *mut u8 {
    let Some(mut entry) = TABLE.with(|t| t.remove(p.addr())) else {
        fatal(
            "realloc of a pointer that is not a live allocation",
            p.addr(),
            old_size,
        )
    };
    if entry.size != old_size {
        fatal(
            "realloc with the wrong size (allocated with the size in parentheses)",
            p.addr(),
            entry.size,
        );
    }
    // SAFETY: `p` is live with `old_size` bytes; on failure it is put back unchanged.
    unsafe {
        let q = if entry.guard_base.is_null() && !guard_pages() {
            heap::realloc(p, old_size, align, new_size)
        } else {
            // Always move, so stale pointers to the old block fault.
            let q = alloc(new_size, align, false);
            if !q.is_null() {
                ptr::copy_nonoverlapping(p, q, old_size.min(new_size));
                release(p, &entry);
                return q;
            }
            q
        };
        if q.is_null() {
            record(&entry);
        } else {
            entry.addr = q.addr();
            entry.size = new_size;
            record(&entry);
        }
        q
    }
}
