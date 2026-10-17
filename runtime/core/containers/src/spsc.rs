//! `SpscQueue<T, N>`: a lock-free ring for one producer thread and one consumer thread.

use core::cell::UnsafeCell;
use core::fmt;
use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use crate::CapacityError;

/// Keeps the producer's and the consumer's index on separate cache lines (128 bytes: adjacent-line
/// prefetch on x86, and Apple cores' line size).
#[repr(align(128))]
struct CachePadded<T>(T);

const PRODUCER: u8 = 1;
const CONSUMER: u8 = 2;

/// A bounded single-producer, single-consumer queue of up to `N` elements, stored inline. Push and
/// pop are wait-free: one atomic load (usually skipped, thanks to a cached copy of the other
/// side's index), a copy, one atomic store.
///
/// The single-producer, single-consumer rule is enforced by types: the queue itself only hands
/// out one [`Producer`] and one [`Consumer`] at a time ([`split`](Self::split), or
/// [`producer`](Self::producer) / [`consumer`](Self::consumer) separately), and pushing or popping
/// needs `&mut` on that half. A dropped half can be taken again. `new` is `const`, so the queue
/// can live in a `static`:
///
/// ```
/// use marlin_containers::SpscQueue;
///
/// static EVENTS: SpscQueue<u32, 64> = SpscQueue::new();
/// let (mut tx, mut rx) = EVENTS.split().unwrap();
/// tx.push(7).unwrap();
/// assert_eq!(rx.pop(), Some(7));
/// ```
///
/// For several producers or consumers use `marlin_sync::Channel` (bounded MPMC).
pub struct SpscQueue<T, const N: usize> {
    /// Next position to read, in `0..2N` (so full and empty differ). Written by the consumer.
    head: CachePadded<AtomicUsize>,
    /// Next position to write, in `0..2N`. Written by the producer.
    tail: CachePadded<AtomicUsize>,
    /// The halves currently handed out: `PRODUCER | CONSUMER` bits.
    taken: AtomicU8,
    slots: [UnsafeCell<MaybeUninit<T>>; N],
}

// SAFETY: a slot is accessed by one side at a time: the producer writes slots outside
// `head..tail` and publishes them with a release store of `tail`; the consumer reads slots inside
// it and frees them with a release store of `head`. Only one producer and one consumer exist.
unsafe impl<T: Send, const N: usize> Sync for SpscQueue<T, N> {}

impl<T, const N: usize> SpscQueue<T, N> {
    pub const fn new() -> Self {
        const {
            assert!(N > 0, "SpscQueue needs room for at least one element");
            assert!(N <= usize::MAX / 2, "SpscQueue capacity too large");
        }
        Self {
            head: CachePadded(AtomicUsize::new(0)),
            tail: CachePadded(AtomicUsize::new(0)),
            taken: AtomicU8::new(0),
            slots: [const { UnsafeCell::new(MaybeUninit::uninit()) }; N],
        }
    }

    #[inline]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Elements queued right now. Exact only when neither side is active.
    pub fn len(&self) -> usize {
        let head = self.head.0.load(Ordering::Acquire);
        let tail = self.tail.0.load(Ordering::Acquire);
        distance(head, tail, N)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Both halves, or `None` if either is already out.
    pub fn split(&self) -> Option<(Producer<'_, T, N>, Consumer<'_, T, N>)> {
        self.taken
            .compare_exchange(0, PRODUCER | CONSUMER, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        Some((Producer::new(self), Consumer::new(self)))
    }

    /// The producing half, or `None` if it is already out.
    pub fn producer(&self) -> Option<Producer<'_, T, N>> {
        let was = self.taken.fetch_or(PRODUCER, Ordering::Acquire);
        (was & PRODUCER == 0).then(|| Producer::new(self))
    }

    /// The consuming half, or `None` if it is already out.
    pub fn consumer(&self) -> Option<Consumer<'_, T, N>> {
        let was = self.taken.fetch_or(CONSUMER, Ordering::Acquire);
        (was & CONSUMER == 0).then(|| Consumer::new(self))
    }

    #[inline(always)]
    fn slot(&self, pos: usize) -> *mut MaybeUninit<T> {
        let i = if pos >= N { pos - N } else { pos };
        self.slots[i].get()
    }
}

/// Elements between positions `head` and `tail` (both in `0..2N`).
#[inline(always)]
const fn distance(head: usize, tail: usize, n: usize) -> usize {
    if tail >= head {
        tail - head
    } else {
        tail + 2 * n - head
    }
}

#[inline(always)]
const fn advance(pos: usize, n: usize) -> usize {
    if pos + 1 == 2 * n { 0 } else { pos + 1 }
}

impl<T, const N: usize> Default for SpscQueue<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Drop for SpscQueue<T, N> {
    fn drop(&mut self) {
        let mut head = *self.head.0.get_mut();
        let tail = *self.tail.0.get_mut();
        while head != tail {
            // SAFETY: positions head..tail hold initialized elements that were never popped, and
            // `&mut self` rules out both halves.
            unsafe { (*self.slot(head)).assume_init_drop() };
            head = advance(head, N);
        }
    }
}

impl<T, const N: usize> fmt::Debug for SpscQueue<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpscQueue")
            .field("len", &self.len())
            .field("capacity", &N)
            .finish()
    }
}

/// The pushing half of an [`SpscQueue`]. `Send`, so it can move to the producer thread.
pub struct Producer<'a, T, const N: usize> {
    queue: &'a SpscQueue<T, N>,
    /// Equal to `queue.tail`, which only this half writes.
    tail: usize,
    /// The consumer's `head` as last seen; it only moves forward, so a stale copy is safe.
    head: usize,
    /// Not `Sync`: two threads must not share one producer.
    _not_sync: PhantomData<*mut ()>,
}

// SAFETY: moving the only producer to another thread keeps a single producer; `T: Send` because
// its values cross to the consumer's thread.
unsafe impl<T: Send, const N: usize> Send for Producer<'_, T, N> {}

impl<'a, T, const N: usize> Producer<'a, T, N> {
    fn new(queue: &'a SpscQueue<T, N>) -> Self {
        Self {
            queue,
            tail: queue.tail.0.load(Ordering::Relaxed),
            head: queue.head.0.load(Ordering::Acquire),
            _not_sync: PhantomData,
        }
    }

    /// Appends `value`, or hands it back if the queue is full.
    #[inline]
    pub fn push(&mut self, value: T) -> Result<(), CapacityError<T>> {
        if distance(self.head, self.tail, N) == N {
            self.head = self.queue.head.0.load(Ordering::Acquire);
            if distance(self.head, self.tail, N) == N {
                return Err(CapacityError(value));
            }
        }
        // SAFETY: the slot at `tail` is outside head..tail, so the consumer is done with it (the
        // acquire load of `head` ordered its read before this write) and will not read it until
        // the release store below.
        unsafe { (*self.queue.slot(self.tail)).write(value) };
        self.tail = advance(self.tail, N);
        self.queue.tail.0.store(self.tail, Ordering::Release);
        Ok(())
    }

    /// Free slots right now (more may open up as the consumer pops).
    pub fn free(&mut self) -> usize {
        self.head = self.queue.head.0.load(Ordering::Acquire);
        N - distance(self.head, self.tail, N)
    }

    pub fn is_full(&mut self) -> bool {
        self.free() == 0
    }

    #[inline]
    pub fn queue(&self) -> &'a SpscQueue<T, N> {
        self.queue
    }
}

impl<T, const N: usize> Drop for Producer<'_, T, N> {
    fn drop(&mut self) {
        self.queue.taken.fetch_and(!PRODUCER, Ordering::Release);
    }
}

impl<T, const N: usize> fmt::Debug for Producer<'_, T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Producer").finish_non_exhaustive()
    }
}

/// The popping half of an [`SpscQueue`]. `Send`, so it can move to the consumer thread.
pub struct Consumer<'a, T, const N: usize> {
    queue: &'a SpscQueue<T, N>,
    /// Equal to `queue.head`, which only this half writes.
    head: usize,
    /// The producer's `tail` as last seen; it only moves forward, so a stale copy is safe.
    tail: usize,
    /// Not `Sync`: two threads must not share one consumer.
    _not_sync: PhantomData<*mut ()>,
}

// SAFETY: as for `Producer`.
unsafe impl<T: Send, const N: usize> Send for Consumer<'_, T, N> {}

impl<'a, T, const N: usize> Consumer<'a, T, N> {
    fn new(queue: &'a SpscQueue<T, N>) -> Self {
        Self {
            queue,
            head: queue.head.0.load(Ordering::Relaxed),
            tail: queue.tail.0.load(Ordering::Acquire),
            _not_sync: PhantomData,
        }
    }

    /// Whether an element is ready, refreshing the cached tail if needed.
    #[inline]
    fn ready(&mut self) -> bool {
        if self.head == self.tail {
            self.tail = self.queue.tail.0.load(Ordering::Acquire);
        }
        self.head != self.tail
    }

    /// Removes the oldest element.
    #[inline]
    pub fn pop(&mut self) -> Option<T> {
        if !self.ready() {
            return None;
        }
        // SAFETY: `head` is inside head..tail: the producer initialized it before its release
        // store of `tail`, which the acquire load in `ready` saw, and will not touch it until
        // the release store below.
        let value = unsafe { (*self.queue.slot(self.head)).assume_init_read() };
        self.head = advance(self.head, N);
        self.queue.head.0.store(self.head, Ordering::Release);
        Some(value)
    }

    /// The oldest element, left in the queue.
    #[inline]
    pub fn peek(&mut self) -> Option<&T> {
        if !self.ready() {
            return None;
        }
        // SAFETY: as in `pop`; the element stays put until this consumer pops it, which needs
        // `&mut self` and so ends the borrow.
        Some(unsafe { (*self.queue.slot(self.head)).assume_init_ref() })
    }

    /// Elements ready right now (more may arrive as the producer pushes).
    pub fn len(&mut self) -> usize {
        self.tail = self.queue.tail.0.load(Ordering::Acquire);
        distance(self.head, self.tail, N)
    }

    pub fn is_empty(&mut self) -> bool {
        !self.ready()
    }

    #[inline]
    pub fn queue(&self) -> &'a SpscQueue<T, N> {
        self.queue
    }
}

impl<T, const N: usize> Drop for Consumer<'_, T, N> {
    fn drop(&mut self) {
        self.queue.taken.fetch_and(!CONSUMER, Ordering::Release);
    }
}

impl<T, const N: usize> fmt::Debug for Consumer<'_, T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Consumer").finish_non_exhaustive()
    }
}
