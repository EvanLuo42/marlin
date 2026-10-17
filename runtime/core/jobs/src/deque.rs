//! The per-worker job queues: lock-free Chase-Lev work-stealing deques.
//!
//! The owner pushes and pops at the bottom (newest first); any thread steals from the top (oldest
//! first). The algorithm and its memory orderings are those of Lê, Pop, Cohen and Zappa Nardelli,
//! "Correct and Efficient Work-Stealing for Weak Memory Models" (PPoPP 2013), figure 1, which
//! proves the C11 orderings used here correct. Comments name its steps.
//!
//! Slots are pairs of atomics rather than plain memory: a thief may read a slot that the owner
//! overwrites after the buffer wrapped around. Such a thief always loses its CAS on `top` and
//! discards what it read, but with plain memory the read itself would be a data race.
//!
//! The buffer grows (doubling) when full and never shrinks. A thief may still be reading a
//! buffer the owner has just replaced, so replaced buffers are kept until the deque is dropped
//! instead of being freed: they hold at most as many slots as the current one, since each is half
//! the size of the next.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::mem;
use core::ptr;
use core::sync::atomic::{AtomicIsize, AtomicPtr, Ordering, fence};

use crate::scheduler::Job;

const INITIAL_CAPACITY: usize = 256;

/// One job, stored as its two words.
struct Slot {
    run: AtomicPtr<()>,
    data: AtomicPtr<()>,
}

struct Buffer {
    /// Capacity - 1; the capacity is a power of two.
    mask: usize,
    slots: Box<[Slot]>,
}

impl Buffer {
    fn new(capacity: usize) -> *mut Buffer {
        debug_assert!(capacity.is_power_of_two());
        let slots = (0..capacity)
            .map(|_| Slot {
                run: AtomicPtr::new(ptr::null_mut()),
                data: AtomicPtr::new(ptr::null_mut()),
            })
            .collect();
        Box::into_raw(Box::new(Buffer {
            mask: capacity - 1,
            slots,
        }))
    }

    fn capacity(&self) -> isize {
        (self.mask + 1) as isize
    }

    fn slot(&self, index: isize) -> &Slot {
        &self.slots[index as usize & self.mask]
    }

    fn write(&self, index: isize, job: Job) {
        let slot = self.slot(index);
        slot.run.store(job.run as *mut (), Ordering::Relaxed);
        slot.data.store(job.data, Ordering::Relaxed);
    }

    /// # Safety
    /// Slot `index` holds a job written by [`write`](Self::write), unless the caller discards
    /// the result without calling it.
    unsafe fn read(&self, index: isize) -> Job {
        let slot = self.slot(index);
        let run = slot.run.load(Ordering::Relaxed);
        Job {
            // SAFETY: the pointer was made from an `unsafe fn(*mut ())` by `write` (forwarded
            // from the caller); function and data pointers have the same size on every target.
            run: unsafe { mem::transmute::<*mut (), unsafe fn(*mut ())>(run) },
            data: slot.data.load(Ordering::Relaxed),
        }
    }
}

/// Keeps `top` and `bottom` on separate cache lines: thieves write one, the owner the other.
#[repr(align(64))]
struct Padded<T>(T);

pub(crate) enum Steal {
    Empty,
    Success(Job),
    /// Lost a race with the owner or another thief; the deque may still hold jobs.
    Retry,
}

pub(crate) struct Deque {
    /// Next index to steal. Only grows.
    top: Padded<AtomicIsize>,
    /// Next index to push; written by the owner only.
    bottom: Padded<AtomicIsize>,
    buffer: AtomicPtr<Buffer>,
    /// Owner only: buffers replaced by a larger one, freed with the deque.
    retired: UnsafeCell<Vec<*mut Buffer>>,
}

// SAFETY: the owner-only parts (`retired`, pushing and popping) are used by one thread at a time,
// as the scheduler guarantees: worker `i`'s deque is pushed and popped only by worker `i`'s thread.
// Everything shared goes through atomics.
unsafe impl Send for Deque {}
// SAFETY: as above.
unsafe impl Sync for Deque {}

impl Deque {
    pub(crate) fn new() -> Self {
        Self {
            top: Padded(AtomicIsize::new(0)),
            bottom: Padded(AtomicIsize::new(0)),
            buffer: AtomicPtr::new(Buffer::new(INITIAL_CAPACITY)),
            retired: UnsafeCell::new(Vec::new()),
        }
    }

    /// Owner only. `push` in the paper.
    ///
    /// # Safety
    /// Called only by the deque's owner thread, never concurrently with `pop`.
    pub(crate) unsafe fn push(&self, job: Job) {
        let b = self.bottom.0.load(Ordering::Relaxed);
        let t = self.top.0.load(Ordering::Acquire);
        let mut buffer = self.buffer.load(Ordering::Relaxed);
        // SAFETY: the current buffer is live; only the owner (us) replaces it.
        if b - t > unsafe { (*buffer).capacity() } - 1 {
            // SAFETY: forwarded from the caller.
            buffer = unsafe { self.grow(buffer, t, b) };
        }
        // SAFETY: as above.
        unsafe { (*buffer).write(b, job) };
        fence(Ordering::Release);
        self.bottom.0.store(b + 1, Ordering::Relaxed);
    }

    /// Owner only: the newest job. `take` in the paper.
    ///
    /// # Safety
    /// As for [`push`](Self::push).
    pub(crate) unsafe fn pop(&self) -> Option<Job> {
        let b = self.bottom.0.load(Ordering::Relaxed) - 1;
        let buffer = self.buffer.load(Ordering::Relaxed);
        self.bottom.0.store(b, Ordering::Relaxed);
        fence(Ordering::SeqCst);
        let t = self.top.0.load(Ordering::Relaxed);
        if t > b {
            // Empty.
            self.bottom.0.store(b + 1, Ordering::Relaxed);
            return None;
        }
        // SAFETY: `t <= b`, so slot `b` holds the job pushed there and no thief can take it
        // unless `t == b`, which the CAS below settles.
        let job = unsafe { (*buffer).read(b) };
        if t < b {
            return Some(job);
        }
        // The last job: race the thieves for it.
        let won = self
            .top
            .0
            .compare_exchange(t, t + 1, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok();
        self.bottom.0.store(b + 1, Ordering::Relaxed);
        won.then_some(job)
    }

    /// Any thread: the oldest job. `steal` in the paper.
    pub(crate) fn steal(&self) -> Steal {
        let t = self.top.0.load(Ordering::Acquire);
        fence(Ordering::SeqCst);
        let b = self.bottom.0.load(Ordering::Acquire);
        if t >= b {
            return Steal::Empty;
        }
        // The paper's `consume`; Rust has none, and acquire is at least as strong.
        let buffer = self.buffer.load(Ordering::Acquire);
        // SAFETY: buffers live as long as the deque. If slot `t` was overwritten or never held
        // the job we want, the CAS fails and the value is discarded.
        let job = unsafe { (*buffer).read(t) };
        match self
            .top
            .0
            .compare_exchange(t, t + 1, Ordering::SeqCst, Ordering::Relaxed)
        {
            Ok(_) => Steal::Success(job),
            Err(_) => Steal::Retry,
        }
    }

    /// Whether the deque looks empty. Only a hint: it may change at once.
    pub(crate) fn is_empty(&self) -> bool {
        self.bottom.0.load(Ordering::Relaxed) <= self.top.0.load(Ordering::Relaxed)
    }

    /// Owner only: copies `top..bottom` into a buffer twice the size and publishes it.
    ///
    /// # Safety
    /// As for [`push`](Self::push); `old` is the current buffer.
    #[cold]
    unsafe fn grow(&self, old: *mut Buffer, t: isize, b: isize) -> *mut Buffer {
        // SAFETY: `old` is live (see `retired`), and only the owner replaces it.
        unsafe {
            let new = Buffer::new(((*old).capacity() as usize) * 2);
            for i in t..b {
                (*new).write(i, (*old).read(i));
            }
            // Thieves that load the new buffer must see the copied slots.
            self.buffer.store(new, Ordering::Release);
            (*self.retired.get()).push(old);
            new
        }
    }
}

impl Drop for Deque {
    fn drop(&mut self) {
        // SAFETY: `&mut self`: no thief or owner is left, and every buffer was made by
        // `Buffer::new` and is freed once.
        unsafe {
            drop(Box::from_raw(*self.buffer.get_mut()));
            for &buffer in self.retired.get_mut().iter() {
                drop(Box::from_raw(buffer));
            }
        }
    }
}
