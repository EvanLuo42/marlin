//! Bounded multi-producer multi-consumer channel in a fixed array.
//!
//! The queue is Dmitry Vyukov's bounded MPMC ring, laid out as in crossbeam's `ArrayQueue`: each
//! slot carries a stamp saying whose turn it is, and `head`/`tail` are claimed with one CAS each.
//! A position packs `lap | index`: `index` is the low bits, below `MARK_BIT`; laps count in
//! multiples of `ONE_LAP`. The `MARK_BIT` of `tail` is the closed flag, so closing and a send's
//! claim are ordered by the same word and no send can slip in after a receiver has seen "closed
//! and empty".
//!
//! Lock-free on the fast path, but not wait-free: a receiver that reaches a slot whose sender has
//! claimed but not yet written it waits (spinning, then yielding) for that sender, and likewise
//! for senders behind a slow receiver.
//!
//! Blocking is layered on top with two event counts, one per direction: a sleeper registers in
//! `*_waiters`, reads `*_seq`, re-checks the queue and sleeps on `*_seq`; the other side bumps it
//! after making progress, but only if someone is registered, so the uncontended path makes no
//! syscall. A SeqCst fence on each side makes the register-then-check and change-then-look
//! sequences meet: either the sleeper sees the change, or the waker sees the sleeper.

use core::cell::UnsafeCell;
use core::fmt;
use core::mem::MaybeUninit;
use core::ops::Deref;
use core::sync::atomic::Ordering::{Acquire, Relaxed, Release, SeqCst};
use core::sync::atomic::{AtomicU32, AtomicUsize, fence};
use core::time::Duration;

use marlin_pal::{futex, thread};

use crate::timeout::{self, Timeout};

struct Slot<T> {
    /// `pos + 1` once the value for position `pos` is written; `pos + ONE_LAP` once it is read
    /// (the position one lap later may then write).
    stamp: AtomicUsize,
    value: UnsafeCell<MaybeUninit<T>>,
}

/// Keeps `head` and `tail` on separate cache lines, so senders and receivers do not contend on
/// one line.
#[repr(align(64))]
struct CachePadded<T>(T);

impl<T> Deref for CachePadded<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

/// A bounded MPMC channel holding up to `N` values inline, with no allocation; `new` is `const`,
/// so a channel can live in a `static`.
///
/// [`send`](Self::send) blocks while the channel is full, [`recv`](Self::recv) while it is empty.
/// After [`close`](Self::close), sends fail and hand the value back, and receivers drain what is
/// left before reporting [`RecvError`]. Values still inside when the channel drops are dropped.
///
/// No endpoints: every operation goes through `&Channel`, and closing is explicit rather than
/// tied to the last sender going away. See the module source for the algorithm.
pub struct Channel<T, const N: usize> {
    head: CachePadded<AtomicUsize>,
    /// Holds `MARK_BIT` once closed.
    tail: CachePadded<AtomicUsize>,
    slots: [Slot<T>; N],
    /// Bumped (when anyone sleeps) after a send, for sleeping receivers.
    recv_seq: AtomicU32,
    recv_waiters: AtomicU32,
    /// Bumped (when anyone sleeps) after a receive, for sleeping senders.
    send_seq: AtomicU32,
    send_waiters: AtomicU32,
}

// SAFETY: values are moved in by one thread and out by another, never shared, so `T: Send` is
// enough for both. Each slot's value is accessed only by the thread whose CAS claimed it.
unsafe impl<T: Send, const N: usize> Send for Channel<T, N> {}
// SAFETY: as above.
unsafe impl<T: Send, const N: usize> Sync for Channel<T, N> {}

impl<T, const N: usize> Channel<T, N> {
    /// The tail bit marking the channel closed; also one past the largest index.
    const MARK_BIT: usize = (N + 1).next_power_of_two();
    /// Added to a position to move to the same index one lap later.
    const ONE_LAP: usize = Self::MARK_BIT * 2;

    /// An empty, open channel.
    ///
    /// # Panics
    ///
    /// At compile time, if `N` is 0.
    pub const fn new() -> Self {
        const { assert!(N > 0, "a channel needs a capacity of at least 1") };
        let mut slots = [const {
            Slot {
                stamp: AtomicUsize::new(0),
                value: UnsafeCell::new(MaybeUninit::uninit()),
            }
        }; N];
        // Lap 0: slot `i` waits for the send at position `i`.
        let mut i = 0;
        while i < N {
            slots[i].stamp = AtomicUsize::new(i);
            i += 1;
        }
        Self {
            head: CachePadded(AtomicUsize::new(0)),
            tail: CachePadded(AtomicUsize::new(0)),
            slots,
            recv_seq: AtomicU32::new(0),
            recv_waiters: AtomicU32::new(0),
            send_seq: AtomicU32::new(0),
            send_waiters: AtomicU32::new(0),
        }
    }

    #[inline]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Values in the channel. Only a hint: it may change immediately.
    pub fn len(&self) -> usize {
        loop {
            let tail = self.tail.load(SeqCst);
            let head = self.head.load(SeqCst);
            // A consistent pair: `tail` did not move while we read `head`.
            if self.tail.load(SeqCst) == tail {
                let tail = tail & !Self::MARK_BIT;
                let head_index = head & (Self::MARK_BIT - 1);
                let tail_index = tail & (Self::MARK_BIT - 1);
                return if head_index < tail_index {
                    tail_index - head_index
                } else if head_index > tail_index {
                    N - head_index + tail_index
                } else if tail == head {
                    0
                } else {
                    N
                };
            }
        }
    }

    /// Whether the channel holds no values. Only a hint: it may change immediately.
    pub fn is_empty(&self) -> bool {
        let head = self.head.load(SeqCst);
        let tail = self.tail.load(SeqCst);
        // `tail` is read last, so if it equals `head` the channel was empty at some point between.
        tail & !Self::MARK_BIT == head
    }

    /// Whether the channel holds `N` values. Only a hint: it may change immediately.
    pub fn is_full(&self) -> bool {
        let tail = self.tail.load(SeqCst);
        let head = self.head.load(SeqCst);
        // `head` is read last, so if it is a lap behind `tail` the channel was full at some point
        // between.
        head.wrapping_add(Self::ONE_LAP) == tail & !Self::MARK_BIT
    }

    /// Whether [`close`](Self::close) has been called.
    #[inline]
    pub fn is_closed(&self) -> bool {
        self.tail.load(SeqCst) & Self::MARK_BIT != 0
    }

    /// Closes the channel: from now on sends fail, and receives fail once the channel is empty.
    /// Wakes every blocked sender and receiver. Returns `false` if it was already closed.
    pub fn close(&self) -> bool {
        let tail = self.tail.fetch_or(Self::MARK_BIT, SeqCst);
        if tail & Self::MARK_BIT != 0 {
            return false;
        }
        // Unconditional: closing is rare, and every sleeper must see it. Release, so a sleeper
        // that reads the bumped count (its fence acquires) also sees the closed mark.
        for seq in [&self.recv_seq, &self.send_seq] {
            seq.fetch_add(1, Release);
            futex::wake_all(seq);
        }
        true
    }

    /// Sends `value`, blocking while the channel is full. Fails, handing `value` back, if the
    /// channel is closed.
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.send_inner(value, None).map_err(|e| match e {
            SendTimeoutError::Closed(value) => SendError(value),
            SendTimeoutError::Timeout(_) => unreachable!("send without a timeout timed out"),
        })
    }

    /// Sends `value` if there is room now.
    pub fn try_send(&self, value: T) -> Result<(), TrySendError<T>> {
        self.push(value)?;
        self.notify(&self.recv_seq, &self.recv_waiters);
        Ok(())
    }

    /// Like [`send`](Self::send), giving up after `timeout`.
    pub fn send_timeout(&self, value: T, timeout: Duration) -> Result<(), SendTimeoutError<T>> {
        self.send_inner(value, Some(&Timeout::start(timeout)))
    }

    /// Receives a value, blocking while the channel is empty. Fails once the channel is closed
    /// and empty.
    pub fn recv(&self) -> Result<T, RecvError> {
        self.recv_inner(None).map_err(|e| match e {
            RecvTimeoutError::Closed => RecvError,
            RecvTimeoutError::Timeout => unreachable!("recv without a timeout timed out"),
        })
    }

    /// Receives a value if one is there now.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let value = self.pop()?;
        self.notify(&self.send_seq, &self.send_waiters);
        Ok(value)
    }

    /// Like [`recv`](Self::recv), giving up after `timeout`.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        self.recv_inner(Some(&Timeout::start(timeout)))
    }

    fn send_inner(
        &self,
        mut value: T,
        timeout: Option<&Timeout>,
    ) -> Result<(), SendTimeoutError<T>> {
        loop {
            match self.try_send(value) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Closed(v)) => return Err(SendTimeoutError::Closed(v)),
                Err(TrySendError::Full(v)) => value = v,
            }
            // Only after a failed attempt, so a sender woken for the last free slot tries it.
            let Some(remaining) = timeout::wait_time(timeout) else {
                return Err(SendTimeoutError::Timeout(value));
            };
            self.sleep(&self.send_seq, &self.send_waiters, remaining, || {
                !self.is_full() || self.is_closed()
            });
        }
    }

    fn recv_inner(&self, timeout: Option<&Timeout>) -> Result<T, RecvTimeoutError> {
        loop {
            match self.try_recv() {
                Ok(value) => return Ok(value),
                Err(TryRecvError::Closed) => return Err(RecvTimeoutError::Closed),
                Err(TryRecvError::Empty) => {}
            }
            let Some(remaining) = timeout::wait_time(timeout) else {
                return Err(RecvTimeoutError::Timeout);
            };
            self.sleep(&self.recv_seq, &self.recv_waiters, remaining, || {
                !self.is_empty() || self.is_closed()
            });
        }
    }

    /// Sleeps on `seq` unless `ready` (checked after registering) already holds. May return
    /// spuriously.
    fn sleep(
        &self,
        seq: &AtomicU32,
        waiters: &AtomicU32,
        timeout: Option<Duration>,
        ready: impl Fn() -> bool,
    ) {
        waiters.fetch_add(1, Relaxed);
        let observed = seq.load(Relaxed);
        // Pairs with the fence in `notify`.
        fence(SeqCst);
        if !ready() {
            futex::wait(seq, observed, timeout);
        }
        waiters.fetch_sub(1, Relaxed);
    }

    /// Wakes one sleeper on `seq`, if any is registered, after this thread changed the queue.
    #[inline]
    fn notify(&self, seq: &AtomicU32, waiters: &AtomicU32) {
        // Pairs with the fence in `sleep`.
        fence(SeqCst);
        if waiters.load(Relaxed) != 0 {
            seq.fetch_add(1, Release);
            futex::wake_one(seq);
        }
    }

    fn push(&self, value: T) -> Result<(), TrySendError<T>> {
        let mut backoff = Backoff::new();
        let mut tail = self.tail.load(Relaxed);
        loop {
            if tail & Self::MARK_BIT != 0 {
                return Err(TrySendError::Closed(value));
            }
            let index = tail & (Self::MARK_BIT - 1);
            let lap = tail & !(Self::ONE_LAP - 1);
            let slot = &self.slots[index];
            let stamp = slot.stamp.load(Acquire);

            if tail == stamp {
                // The slot is free for this lap: claim the position.
                let new_tail = if index + 1 < N {
                    tail + 1
                } else {
                    lap.wrapping_add(Self::ONE_LAP)
                };
                match self
                    .tail
                    .compare_exchange_weak(tail, new_tail, SeqCst, Relaxed)
                {
                    Ok(_) => {
                        // SAFETY: the CAS gave this thread position `tail`, and the stamp says the
                        // previous lap's value was read out, so nobody else touches the slot until
                        // the stamp below hands it to the receiver of this position.
                        unsafe { (*slot.value.get()).write(value) };
                        slot.stamp.store(tail.wrapping_add(1), Release);
                        return Ok(());
                    }
                    Err(t) => {
                        tail = t;
                        backoff.spin();
                    }
                }
            } else if stamp.wrapping_add(Self::ONE_LAP) == tail.wrapping_add(1) {
                // The slot still holds last lap's value: full, unless a receiver is mid-read.
                fence(SeqCst);
                let head = self.head.load(Relaxed);
                if head.wrapping_add(Self::ONE_LAP) == tail {
                    return Err(TrySendError::Full(value));
                }
                backoff.spin();
                tail = self.tail.load(Relaxed);
            } else {
                // Another sender claimed this position and has not finished; reload.
                backoff.snooze();
                tail = self.tail.load(Relaxed);
            }
        }
    }

    fn pop(&self) -> Result<T, TryRecvError> {
        let mut backoff = Backoff::new();
        let mut head = self.head.load(Relaxed);
        loop {
            let index = head & (Self::MARK_BIT - 1);
            let lap = head & !(Self::ONE_LAP - 1);
            let slot = &self.slots[index];
            let stamp = slot.stamp.load(Acquire);

            if head.wrapping_add(1) == stamp {
                // The slot holds this lap's value: claim the position.
                let new_head = if index + 1 < N {
                    head + 1
                } else {
                    lap.wrapping_add(Self::ONE_LAP)
                };
                match self
                    .head
                    .compare_exchange_weak(head, new_head, SeqCst, Relaxed)
                {
                    Ok(_) => {
                        // SAFETY: the CAS gave this thread position `head`, and the stamp (read
                        // with Acquire) says its sender finished writing. The stamp below hands
                        // the slot to the next lap's sender only after the read.
                        let value = unsafe { (*slot.value.get()).assume_init_read() };
                        slot.stamp.store(head.wrapping_add(Self::ONE_LAP), Release);
                        return Ok(value);
                    }
                    Err(h) => {
                        head = h;
                        backoff.spin();
                    }
                }
            } else if stamp == head {
                // Not yet written for this lap: empty, unless a sender is mid-write.
                fence(SeqCst);
                let tail = self.tail.load(Relaxed);
                if tail & !Self::MARK_BIT == head {
                    return Err(if tail & Self::MARK_BIT != 0 {
                        TryRecvError::Closed
                    } else {
                        TryRecvError::Empty
                    });
                }
                backoff.spin();
                head = self.head.load(Relaxed);
            } else {
                // Another receiver claimed this position and has not finished; reload.
                backoff.snooze();
                head = self.head.load(Relaxed);
            }
        }
    }
}

impl<T, const N: usize> Default for Channel<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Drop for Channel<T, N> {
    fn drop(&mut self) {
        // `&mut self`: no sender or receiver is mid-operation, so `pop` never waits.
        while self.pop().is_ok() {}
    }
}

impl<T, const N: usize> fmt::Debug for Channel<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel")
            .field("len", &self.len())
            .field("capacity", &N)
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

/// Spins with growing pauses, then yields the time slice. For the short waits on another thread
/// that is between its claim and its write (or read) of a slot.
struct Backoff {
    step: u32,
}

impl Backoff {
    const SPIN_STEPS: u32 = 6;

    fn new() -> Self {
        Self { step: 0 }
    }

    /// After a lost CAS: the winner is making progress, so a short pause is enough.
    fn spin(&mut self) {
        for _ in 0..1 << self.step.min(Self::SPIN_STEPS) {
            core::hint::spin_loop();
        }
        if self.step <= Self::SPIN_STEPS {
            self.step += 1;
        }
    }

    /// While waiting on another thread's unfinished operation, which may have been preempted.
    fn snooze(&mut self) {
        if self.step <= Self::SPIN_STEPS {
            for _ in 0..1 << self.step {
                core::hint::spin_loop();
            }
            self.step += 1;
        } else {
            thread::yield_now();
        }
    }
}

/// [`Channel::send`] failed because the channel is closed; holds the value that was not sent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SendError<T>(pub T);

impl<T> SendError<T> {
    #[inline]
    pub fn into_inner(self) -> T {
        self.0
    }
}

/// Why [`Channel::try_send`] failed; holds the value that was not sent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TrySendError<T> {
    Full(T),
    Closed(T),
}

impl<T> TrySendError<T> {
    #[inline]
    pub fn into_inner(self) -> T {
        match self {
            Self::Full(value) | Self::Closed(value) => value,
        }
    }
}

/// Why [`Channel::send_timeout`] failed; holds the value that was not sent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SendTimeoutError<T> {
    Timeout(T),
    Closed(T),
}

impl<T> SendTimeoutError<T> {
    #[inline]
    pub fn into_inner(self) -> T {
        match self {
            Self::Timeout(value) | Self::Closed(value) => value,
        }
    }
}

// Debug without `T: Debug`, so `unwrap` works on any channel.
impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SendError(..)")
    }
}

impl<T> fmt::Debug for TrySendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Full(_) => "Full(..)",
            Self::Closed(_) => "Closed(..)",
        })
    }
}

impl<T> fmt::Debug for SendTimeoutError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Timeout(_) => "Timeout(..)",
            Self::Closed(_) => "Closed(..)",
        })
    }
}

/// [`Channel::recv`] failed because the channel is closed and empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecvError;

/// Why [`Channel::try_recv`] failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TryRecvError {
    Empty,
    /// Closed and empty.
    Closed,
}

/// Why [`Channel::recv_timeout`] failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecvTimeoutError {
    Timeout,
    /// Closed and empty.
    Closed,
}
