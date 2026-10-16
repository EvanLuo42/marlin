//! The hot path: per-thread single-producer/single-consumer event rings.
//!
//! A thread's first zone takes a buffer: a free one left by an exited thread, or a new one that
//! is pushed onto a global list and never freed. After that, recording is a clock read and a
//! push into the thread's own ring. The collector (under its mutex) is the only consumer.

use alloc::boxed::Box;
use alloc::string::String;
use core::cell::{Cell, UnsafeCell};
use core::mem::MaybeUninit;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use marlin_std::thread::{LocalKey, current};
use marlin_std::time::Instant;

use crate::ZoneSite;

/// Capacity of a thread's ring, in events, between two frame marks. 24 bytes each.
pub const EVENTS_PER_THREAD: usize = 1 << 15;
const MASK: usize = EVENTS_PER_THREAD - 1;

/// Zone ids are `thread << ID_SHIFT | per-thread counter`; threads start at 1, so ids are never 0.
const ID_SHIFT: u32 = 40;
const COUNTER_MASK: u64 = (1 << ID_SHIFT) - 1;

#[derive(Clone, Copy)]
pub(crate) struct Event {
    pub(crate) ticks: u64,
    pub(crate) id: u64,
    /// `None` for an end.
    pub(crate) site: Option<&'static ZoneSite>,
}

const FREE: u32 = 0;
const CLAIMING: u32 = 1;
const OWNED: u32 = 2;
/// The owner exited; the collector frees the buffer once it has drained it.
const RETIRED: u32 = 3;

#[repr(align(64))]
struct Padded<T>(T);

pub(crate) struct ThreadBuffer {
    /// Written by the producer only.
    head: Padded<AtomicUsize>,
    /// Written by the consumer only.
    tail: Padded<AtomicUsize>,
    /// Owner-only: the next zone id.
    next_id: UnsafeCell<u64>,
    dropped: AtomicU64,
    state: AtomicU32,
    /// The owning thread's record (see `ThreadInfo`), unique per thread for the process.
    thread: AtomicU32,
    os_id: AtomicU64,
    /// Next in `BUFFERS`; immutable once published.
    next: *const ThreadBuffer,
    events: Box<[UnsafeCell<MaybeUninit<Event>>]>,
}

// SAFETY: `next_id` is touched only by the owning thread; ring slots are handed between producer
// and consumer through the Release/Acquire pairs on `head` and `tail`.
unsafe impl Sync for ThreadBuffer {}

impl ThreadBuffer {
    /// Producer side. Never blocks: a full ring drops the event.
    #[inline]
    fn push(&self, event: Event) {
        let head = self.head.0.load(Ordering::Relaxed);
        let tail = self.tail.0.load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= EVENTS_PER_THREAD {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // SAFETY: slot `head` is outside `tail..head`, so the consumer is not reading it, and only
        // the owner produces.
        unsafe { (*self.events[head & MASK].get()).write(event) };
        self.head.0.store(head.wrapping_add(1), Ordering::Release);
    }

    /// Consumer side: passes events stamped at or before `limit` to `f`, with their position in
    /// the buffer, which orders them exactly where ticks may tie.
    ///
    /// # Safety
    /// Only one consumer at a time (the collector's mutex).
    pub(crate) unsafe fn drain(&self, limit: u64, mut f: impl FnMut(u64, Event)) {
        let head = self.head.0.load(Ordering::Acquire);
        let mut tail = self.tail.0.load(Ordering::Relaxed);
        while tail != head {
            // SAFETY: `tail..head` was published by the Release store of `head`.
            let event = unsafe { (*self.events[tail & MASK].get()).assume_init() };
            if event.ticks > limit {
                break;
            }
            f(tail as u64, event);
            tail = tail.wrapping_add(1);
        }
        self.tail.0.store(tail, Ordering::Release);
    }

    fn is_empty(&self) -> bool {
        self.head.0.load(Ordering::Acquire) == self.tail.0.load(Ordering::Relaxed)
    }
}

/// Every buffer ever made, newest first.
static BUFFERS: AtomicPtr<ThreadBuffer> = AtomicPtr::new(ptr::null_mut());
static NEXT_THREAD: AtomicU32 = AtomicU32::new(1);

/// Marks a thread whose exit destructor has run; it records nothing more.
const DEAD: *const ThreadBuffer = ptr::without_provenance(1);

#[thread_local]
static CURRENT: Cell<*const ThreadBuffer> = Cell::new(ptr::null());

// Fibers migrate between threads: never let the compiler reuse CURRENT's address across a wait.
#[inline(never)]
fn current_buffer() -> *const ThreadBuffer {
    CURRENT.get()
}

#[inline(never)]
fn set_current_buffer(buffer: *const ThreadBuffer) {
    CURRENT.set(buffer);
}

/// Retires the thread's buffer when the thread exits.
struct Release(Cell<*const ThreadBuffer>);

impl Drop for Release {
    fn drop(&mut self) {
        let buffer = self.0.get();
        set_current_buffer(DEAD);
        if !buffer.is_null() {
            // SAFETY: buffers are never freed.
            unsafe { (*buffer).state.store(RETIRED, Ordering::Release) };
        }
    }
}

static RELEASE: LocalKey<Release> = LocalKey::new(|| Release(Cell::new(ptr::null())));

/// The calling thread's buffer, taking one on first use; `None` while the thread exits.
#[inline]
fn buffer() -> Option<&'static ThreadBuffer> {
    let mut buffer = current_buffer();
    if buffer.is_null() {
        buffer = register();
    } else if buffer == DEAD {
        return None;
    }
    // SAFETY: buffers are never freed, and this one is owned by the calling thread.
    Some(unsafe { &*buffer })
}

#[cold]
#[inline(never)]
fn register() -> *const ThreadBuffer {
    let mut p = BUFFERS.load(Ordering::Acquire);
    while !p.is_null() {
        // SAFETY: buffers are never freed, and `next` is immutable once published.
        let buffer = unsafe { &*p };
        if buffer
            .state
            .compare_exchange(FREE, CLAIMING, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            claim(buffer);
            return p;
        }
        p = buffer.next.cast_mut();
    }

    let events = (0..EVENTS_PER_THREAD)
        .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
        .collect();
    let buffer = Box::leak(Box::new(ThreadBuffer {
        head: Padded(AtomicUsize::new(0)),
        tail: Padded(AtomicUsize::new(0)),
        next_id: UnsafeCell::new(0),
        dropped: AtomicU64::new(0),
        state: AtomicU32::new(CLAIMING),
        thread: AtomicU32::new(0),
        os_id: AtomicU64::new(0),
        next: ptr::null(),
        events,
    }));
    let mut head = BUFFERS.load(Ordering::Relaxed);
    loop {
        buffer.next = head;
        match BUFFERS.compare_exchange_weak(head, buffer, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => break,
            Err(now) => head = now,
        }
    }
    claim(buffer);
    buffer
}

/// Makes `buffer` (in `CLAIMING`) the calling thread's.
fn claim(buffer: &'static ThreadBuffer) {
    let thread = NEXT_THREAD.fetch_add(1, Ordering::Relaxed);
    // SAFETY: in `CLAIMING` nobody else touches `next_id`.
    unsafe { *buffer.next_id.get() = u64::from(thread) << ID_SHIFT };
    buffer.thread.store(thread, Ordering::Relaxed);
    buffer
        .os_id
        .store(current::id().as_u64(), Ordering::Relaxed);
    buffer.state.store(OWNED, Ordering::Release);
    set_current_buffer(buffer);
    RELEASE.with(|r| r.0.set(buffer));
}

#[inline]
pub(crate) fn begin(site: &'static ZoneSite) -> u64 {
    let Some(buffer) = buffer() else { return 0 };
    // SAFETY: only the owning thread touches `next_id`.
    let id = unsafe {
        let next = &mut *buffer.next_id.get();
        *next = (*next & !COUNTER_MASK) | (next.wrapping_add(1) & COUNTER_MASK);
        *next
    };
    buffer.push(Event {
        ticks: Instant::now().as_ticks(),
        id,
        site: Some(site),
    });
    id
}

/// Records on the thread that ends the zone, which may not be the one that began it.
#[inline]
pub(crate) fn end(id: u64) {
    let ticks = Instant::now().as_ticks();
    if let Some(buffer) = buffer() {
        buffer.push(Event {
            ticks,
            id,
            site: None,
        });
    }
}

/// The calling thread's record id, registering it if needed; 0 while the thread exits.
pub(crate) fn current_thread() -> u32 {
    buffer().map_or(0, |b| b.thread.load(Ordering::Relaxed))
}

/// Names the calling thread in statistics and traces. Costs a lock; call it once per thread.
pub fn set_thread_name(name: &str) {
    if !crate::ENABLED {
        return;
    }
    let thread = current_thread();
    if thread != 0 {
        crate::collector::name_thread(thread, current::id().as_u64(), String::from(name));
    }
}

/// What the collector learns from one buffer.
pub(crate) struct Drained {
    pub(crate) thread: u32,
    pub(crate) os_id: u64,
    pub(crate) dropped: u64,
}

/// Drains every buffer up to `limit`.
///
/// # Safety
/// Only one caller at a time (the collector's mutex).
pub(crate) unsafe fn drain_all(
    limit: u64,
    mut on_buffer: impl FnMut(Drained),
    mut on_event: impl FnMut(u32, u64, Event),
) {
    let mut p = BUFFERS.load(Ordering::Acquire);
    while !p.is_null() {
        // SAFETY: buffers are never freed, and `next` is immutable once published.
        let buffer = unsafe { &*p };
        p = buffer.next.cast_mut();
        let state = buffer.state.load(Ordering::Acquire);
        if state != OWNED && state != RETIRED {
            continue;
        }
        let thread = buffer.thread.load(Ordering::Relaxed);
        on_buffer(Drained {
            thread,
            os_id: buffer.os_id.load(Ordering::Relaxed),
            dropped: buffer.dropped.swap(0, Ordering::Relaxed),
        });
        // SAFETY: the caller is the only consumer.
        unsafe { buffer.drain(limit, |seq, e| on_event(thread, seq, e)) };
        // Only the collector leaves RETIRED, so the owner is gone and nothing more arrives.
        if state == RETIRED && buffer.is_empty() {
            buffer.state.store(FREE, Ordering::Release);
        }
    }
}
