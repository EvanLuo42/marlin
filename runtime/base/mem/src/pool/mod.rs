//! Fixed-size pools: blocks of one layout, from one reserved range, recycled through a free list.
//!
//! [`Pool`] reserves address space for its capacity up front and commits it in
//! [`COMMIT_GRANULE`] steps as blocks are first handed out, like [`Arena`](crate::Arena). Freed
//! blocks go onto an intrusive free list (the link lives in the free block) and are handed out
//! again most recently freed first, while still warm in cache. Allocation and free take a short
//! lock and never call the OS once the memory is committed; any thread may free any block.
//!
//! - [`Pool`]: untyped blocks; `&Pool` is also an [`Allocator`] for layouts that fit a block, so
//!   `Box::new_in(value, &pool)` works.
//! - [`TypedPool`]: blocks of one `T`, moved in on allocation and dropped (or moved out) on free.
//!
//! The pool never drops what its blocks hold: dropping a `Pool` or `TypedPool` releases the memory
//! of blocks still in use without running their destructors.

mod typed;

pub use typed::TypedPool;

use core::alloc::{AllocError, Allocator, Layout};
use core::fmt;
use core::ptr::{NonNull, null_mut};
use core::sync::atomic::{AtomicUsize, Ordering};

use marlin_pal::{Error, ErrorKind, Protection, vmem};

pub use crate::arena::COMMIT_GRANULE;
use crate::lock::Locked;

struct FreeBlock {
    next: *mut FreeBlock,
}

struct State {
    free: *mut FreeBlock,
    /// Blocks handed out at least once, from the base; all blocks past them are untouched.
    carved: usize,
}

/// A pool of fixed-size blocks. See the [module docs](self).
pub struct Pool {
    base: NonNull<u8>,
    reserved: usize,
    /// Distance between blocks: the block size rounded up to the alignment.
    stride: usize,
    align: usize,
    capacity: usize,
    in_use: AtomicUsize,
    /// Bytes committed from the base; a multiple of the granule (or `reserved`). Written under
    /// the lock.
    committed: AtomicUsize,
    state: Locked<State>,
}

// SAFETY: the pool owns its reservation; the free list and carving are reached only under the
// lock, and blocks are handed to one owner at a time.
unsafe impl Send for Pool {}
// SAFETY: as above.
unsafe impl Sync for Pool {}

impl Pool {
    /// Reserves room for at least `capacity` blocks of `block`'s layout (more if the OS's
    /// reservation granularity leaves room). Blocks are at least pointer-sized and -aligned.
    /// Nothing is committed until used.
    pub fn new(block: Layout, capacity: usize) -> marlin_pal::Result<Self> {
        let invalid = || Error::from_kind(ErrorKind::InvalidInput);
        let align = block.align().max(align_of::<FreeBlock>());
        let stride = block
            .size()
            .max(size_of::<FreeBlock>())
            .next_multiple_of(align);
        let reserved = stride
            .checked_mul(capacity.max(1))
            .and_then(|bytes| bytes.checked_next_multiple_of(vmem::allocation_granularity()))
            .ok_or_else(invalid)?;
        // Blocks are aligned because the base is and the stride is a multiple of the alignment.
        let base = if align > vmem::allocation_granularity() {
            vmem::reserve_aligned(reserved, align)?
        } else {
            vmem::reserve(reserved)?
        };
        Ok(Self {
            base,
            reserved,
            stride,
            align,
            capacity: reserved / stride,
            in_use: AtomicUsize::new(0),
            committed: AtomicUsize::new(0),
            state: Locked::new(State {
                free: null_mut(),
                carved: 0,
            }),
        })
    }

    /// The layout of every block: what the pool was made for, rounded up.
    #[inline]
    pub fn block_layout(&self) -> Layout {
        // SAFETY: `stride` is a non-zero multiple of the power-of-two `align`, and fit when the
        // reservation was made.
        unsafe { Layout::from_size_align_unchecked(self.stride, self.align) }
    }

    /// Blocks the pool can hold.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Blocks handed out and not yet freed.
    #[inline]
    pub fn len(&self) -> usize {
        self.in_use.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes of physical memory committed.
    #[inline]
    pub fn committed(&self) -> usize {
        self.committed.load(Ordering::Relaxed)
    }

    /// Whether `ptr` is the start of one of this pool's blocks (live or not).
    #[inline]
    pub fn contains(&self, ptr: *const u8) -> bool {
        let offset = ptr.addr().wrapping_sub(self.base.as_ptr().addr());
        offset < self.capacity * self.stride && offset.is_multiple_of(self.stride)
    }

    /// A block, or `None` once all `capacity` blocks are in use (or the OS is out of memory). Its
    /// contents are unspecified.
    #[inline]
    pub fn alloc(&self) -> Option<NonNull<u8>> {
        let p = self.state.with(|state| {
            let head = state.free;
            if head.is_null() {
                self.carve(state)
            } else {
                // SAFETY: free blocks are committed and hold their link.
                state.free = unsafe { (*head).next };
                head.cast()
            }
        });
        let p = NonNull::new(p)?;
        self.in_use.fetch_add(1, Ordering::Relaxed);
        Some(p)
    }

    /// The next untouched block, committing memory for it as needed.
    #[cold]
    fn carve(&self, state: &mut State) -> *mut u8 {
        if state.carved == self.capacity {
            return null_mut();
        }
        let end = (state.carved + 1) * self.stride;
        let committed = self.committed.load(Ordering::Relaxed);
        if end > committed {
            let target = end.next_multiple_of(COMMIT_GRANULE).min(self.reserved);
            // SAFETY: `committed..target` is granule-aligned (or ends the reservation) and inside
            // it.
            let commit = unsafe {
                vmem::commit(
                    self.base.add(committed),
                    target - committed,
                    Protection::ReadWrite,
                )
            };
            if commit.is_err() {
                return null_mut();
            }
            self.committed.store(target, Ordering::Relaxed);
        }
        // SAFETY: block `carved` lies inside the committed part of the reservation.
        let p = unsafe { self.base.as_ptr().add(state.carved * self.stride) };
        state.carved += 1;
        p
    }

    /// Returns a block to the pool.
    ///
    /// # Safety
    /// `ptr` came from this pool's [`alloc`](Self::alloc), is not used afterwards and is freed
    /// once.
    #[inline]
    pub unsafe fn free(&self, ptr: NonNull<u8>) {
        debug_assert!(
            self.contains(ptr.as_ptr()),
            "freeing a block of another pool"
        );
        let block = ptr.as_ptr().cast::<FreeBlock>();
        self.state.with(|state| {
            // SAFETY: the block is the pool's again (the caller), and large enough for the link.
            unsafe { (*block).next = state.free };
            state.free = block;
        });
        self.in_use.fetch_sub(1, Ordering::Relaxed);
    }

    /// Decommits the memory past the last block in use (rounded up to the granule), or all of it
    /// when no block is in use. Free blocks below the last live one keep their memory.
    ///
    /// Also sorts the free list by address, so blocks are handed out lowest first and the pool
    /// tends to stay compact.
    pub fn trim(&mut self) {
        let in_use = *self.in_use.get_mut();
        let (base, stride) = (self.base.as_ptr(), self.stride);
        let state = self.state.get_mut();
        if in_use == 0 {
            state.free = null_mut();
            state.carved = 0;
        } else {
            // SAFETY: `&mut self`: the free list is ours to relink.
            unsafe {
                let mut head = sort_descending(state.free);
                // Free blocks at the top of the carved range become untouched again.
                while !head.is_null()
                    && state.carved > 0
                    && head.addr() == base.addr() + (state.carved - 1) * stride
                {
                    head = (*head).next;
                    state.carved -= 1;
                }
                state.free = reverse(head);
            }
        }
        let keep = (state.carved * stride)
            .next_multiple_of(COMMIT_GRANULE)
            .min(self.reserved);
        let committed = *self.committed.get_mut();
        if keep < committed {
            // SAFETY: `keep..committed` is granule-aligned, committed, and holds no live or free
            // block.
            if unsafe { vmem::decommit(self.base.add(keep), committed - keep) }.is_ok() {
                *self.committed.get_mut() = keep;
            }
        }
    }
}

/// Merges two lists sorted by descending address.
///
/// # Safety
/// Both are valid, disjoint free lists.
unsafe fn merge(mut a: *mut FreeBlock, mut b: *mut FreeBlock) -> *mut FreeBlock {
    let mut head = null_mut();
    let mut tail: *mut *mut FreeBlock = &mut head;
    // SAFETY: every node is a valid free block.
    unsafe {
        while !a.is_null() && !b.is_null() {
            let node = if a.addr() > b.addr() { a } else { b };
            if node == a {
                a = (*a).next;
            } else {
                b = (*b).next;
            }
            *tail = node;
            tail = &raw mut (*node).next;
        }
        *tail = if a.is_null() { b } else { a };
    }
    head
}

/// Sorts a free list by descending address: a bottom-up merge sort, without allocating.
///
/// # Safety
/// `list` is a valid free list.
unsafe fn sort_descending(mut list: *mut FreeBlock) -> *mut FreeBlock {
    // `runs[i]` is empty or a sorted run of 2^i blocks.
    let mut runs = [null_mut::<FreeBlock>(); usize::BITS as usize];
    // SAFETY: every node is a valid free block.
    unsafe {
        while !list.is_null() {
            let mut run = list;
            list = (*list).next;
            (*run).next = null_mut();
            let mut i = 0;
            while !runs[i].is_null() {
                run = merge(runs[i], run);
                runs[i] = null_mut();
                i += 1;
            }
            runs[i] = run;
        }
        runs.into_iter()
            .fold(null_mut(), |sorted, run| merge(run, sorted))
    }
}

/// # Safety
/// `list` is a valid free list.
unsafe fn reverse(mut list: *mut FreeBlock) -> *mut FreeBlock {
    let mut reversed = null_mut();
    // SAFETY: every node is a valid free block.
    unsafe {
        while !list.is_null() {
            let next = (*list).next;
            (*list).next = reversed;
            reversed = list;
            list = next;
        }
    }
    reversed
}

impl Drop for Pool {
    fn drop(&mut self) {
        // SAFETY: the reservation is ours; blocks still in use are the caller's loss (see the
        // module docs), never accessed through the pool again.
        let _ = unsafe { vmem::release(self.base, self.reserved) };
    }
}

impl fmt::Debug for Pool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pool")
            .field("block", &self.block_layout())
            .field("len", &self.len())
            .field("capacity", &self.capacity())
            .field("committed", &self.committed())
            .finish()
    }
}

impl Pool {
    #[inline]
    fn fits(&self, layout: Layout) -> bool {
        layout.size() <= self.stride && layout.align() <= self.align
    }
}

#[inline]
fn empty(layout: Layout) -> NonNull<[u8]> {
    NonNull::slice_from_raw_parts(layout.dangling_ptr(), 0)
}

// SAFETY: blocks are unique until freed and valid for `stride` bytes at `align`; layouts that do
// not fit are refused, zero-sized ones get a dangling pointer. Resizing within a block keeps it.
unsafe impl Allocator for &Pool {
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if !self.fits(layout) {
            return Err(AllocError);
        }
        if layout.size() == 0 {
            return Ok(empty(layout));
        }
        let p = self.alloc().ok_or(AllocError)?;
        Ok(NonNull::slice_from_raw_parts(p, layout.size()))
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        if layout.size() != 0 {
            // SAFETY: a block of this pool (the caller).
            unsafe { self.free(ptr) }
        }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old: Layout,
        new: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        if old.size() == 0 {
            return self.allocate(new);
        }
        if self.fits(new) && ptr.addr().get().is_multiple_of(new.align()) {
            return Ok(NonNull::slice_from_raw_parts(ptr, new.size()));
        }
        Err(AllocError)
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old: Layout,
        new: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        if new.size() == 0 {
            // SAFETY: forwarded from the caller.
            unsafe { self.deallocate(ptr, old) };
            return Ok(empty(new));
        }
        if ptr.addr().get().is_multiple_of(new.align()) {
            return Ok(NonNull::slice_from_raw_parts(ptr, new.size()));
        }
        Err(AllocError)
    }
}
