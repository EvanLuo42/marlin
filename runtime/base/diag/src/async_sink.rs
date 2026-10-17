//! A sink that hands formatted lines to a background thread.

use alloc::boxed::Box;
use alloc::vec;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use marlin_pal::futex;
use marlin_pal::thread::{self, JoinHandle, ThreadStart};

use crate::Level;
use crate::lock::Locked;
use crate::ring::{copy_in, copy_out};
use crate::sink::{LINE_LEN, LineSink, Record, Sink, format_line};

/// Each entry in the ring: level, then the line's length (little-endian u16), then the line.
const HEADER: usize = 3;

struct Queue {
    bytes: Box<[u8]>,
    /// Bytes ever pushed and popped; `head - tail` are queued.
    head: u64,
    tail: u64,
    /// Entries ever pushed.
    pushed: u64,
    /// Set by `stop`; from then on, records bypass the queue.
    stopped: bool,
}

struct Shared {
    queue: Locked<Queue>,
    out: &'static dyn LineSink,
    dropped: AtomicU64,
    /// Entries the worker has finished writing.
    written: AtomicU64,
    /// Bumped on every push while the worker sleeps; the worker's futex word.
    wake: AtomicU32,
    sleeping: AtomicBool,
    /// Bumped when `written` advances while `flushers > 0`; the flushers' futex word.
    progress: AtomicU32,
    flushers: AtomicU32,
    worker_id: AtomicU64,
}

impl Shared {
    fn wake_worker(&self) {
        if self.sleeping.load(Ordering::SeqCst) {
            self.wake.fetch_add(1, Ordering::SeqCst);
            futex::wake_one(&self.wake);
        }
    }

    /// The oldest entry, copied into `line`: its level and length.
    fn pop(&self, line: &mut [u8; LINE_LEN]) -> Option<(Level, usize)> {
        self.queue.with(|queue| {
            if queue.head == queue.tail {
                return None;
            }
            let mut header = [0u8; HEADER];
            copy_out(&queue.bytes, queue.tail, &mut header);
            let len = usize::from(u16::from_le_bytes([header[1], header[2]])).min(LINE_LEN);
            copy_out(&queue.bytes, queue.tail + HEADER as u64, &mut line[..len]);
            queue.tail += (HEADER + len) as u64;
            Some((Level::from_u8(header[0]), len))
        })
    }

    /// Checks the queue once drained: `Exit` if stopped, `Sleep` (with `sleeping` set) if still
    /// empty.
    fn idle(&self) -> Idle {
        self.queue.with(|queue| {
            if queue.head != queue.tail {
                Idle::Work
            } else if queue.stopped {
                Idle::Exit
            } else {
                // Under the lock: a push after this sees the flag and wakes us; one before it
                // left the queue non-empty.
                self.sleeping.store(true, Ordering::SeqCst);
                Idle::Sleep
            }
        })
    }

    fn run(&self) {
        self.worker_id
            .store(thread::current_id(), Ordering::Relaxed);
        let mut line = [0u8; LINE_LEN];
        loop {
            while let Some((level, len)) = self.pop(&mut line) {
                // Entries are whole `&str`s, so this always succeeds.
                if let Ok(text) = core::str::from_utf8(&line[..len]) {
                    self.out.write_line(level, text);
                }
                self.written.fetch_add(1, Ordering::SeqCst);
                if self.flushers.load(Ordering::SeqCst) > 0 {
                    self.progress.fetch_add(1, Ordering::SeqCst);
                    futex::wake_all(&self.progress);
                }
            }
            // Loaded before the check, so a push (and its bump) after the check makes the wait
            // return at once.
            let seen = self.wake.load(Ordering::SeqCst);
            match self.idle() {
                Idle::Work => {}
                Idle::Exit => return,
                Idle::Sleep => {
                    futex::wait(&self.wake, seen, None);
                    self.sleeping.store(false, Ordering::SeqCst);
                }
            }
        }
    }
}

enum Idle {
    Work,
    Sleep,
    Exit,
}

#[repr(C)]
struct Start {
    header: ThreadStart,
    shared: NonNull<Shared>,
}

/// Aborts if a destination panics on the worker, so unwinding never crosses the OS entry point.
struct AbortOnUnwind;

impl Drop for AbortOnUnwind {
    fn drop(&mut self) {
        marlin_pal::process::abort();
    }
}

/// # Safety
///
/// `start` must be the leaked `Start` made by [`AsyncSink::new`], passed once.
unsafe fn worker(start: NonNull<ThreadStart>) {
    // SAFETY: per the contract we own the leaked `Start`, whose first field is the header.
    let start = unsafe { Box::from_raw(start.cast::<Start>().as_ptr()) };
    thread::set_current_name("marlin-log");
    let guard = AbortOnUnwind;
    // SAFETY: `AsyncSink` frees `Shared` only after joining this thread.
    unsafe { start.shared.as_ref() }.run();
    core::mem::forget(guard);
}

/// Formats records on the logging thread into a fixed byte ring, and writes them to a
/// [`LineSink`] from a background thread, so logging never waits on the destination.
///
/// When the ring is full, records are dropped and counted ([`dropped`](Self::dropped)) instead of
/// blocking. [`flush`](Sink::flush) waits until everything logged before it is written, then
/// flushes the destination. [`stop`](Self::stop) drains the ring and joins the worker; after it,
/// records are written to the destination on the logging thread. Dropping the sink stops it.
pub struct AsyncSink {
    shared: NonNull<Shared>,
    worker: Locked<Option<JoinHandle>>,
    /// The worker could not be joined, so `Shared` must outlive the sink.
    lost: AtomicBool,
}

// SAFETY: `Shared` holds a `'static` `Sync` destination, atomics and lock-protected state, so any
// thread may use it through a shared reference; the worker handle is behind a lock.
unsafe impl Send for AsyncSink {}
// SAFETY: as above.
unsafe impl Sync for AsyncSink {}

impl AsyncSink {
    /// Starts a worker writing to `out`, with a ring of `capacity` bytes. Each record takes its
    /// line length plus 3 bytes.
    pub fn new(out: &'static dyn LineSink, capacity: usize) -> Result<Self, marlin_pal::Error> {
        let shared = NonNull::from(Box::leak(Box::new(Shared {
            queue: Locked::new(Queue {
                bytes: vec![0; capacity].into_boxed_slice(),
                head: 0,
                tail: 0,
                pushed: 0,
                stopped: false,
            }),
            out,
            dropped: AtomicU64::new(0),
            written: AtomicU64::new(0),
            wake: AtomicU32::new(0),
            sleeping: AtomicBool::new(false),
            progress: AtomicU32::new(0),
            flushers: AtomicU32::new(0),
            worker_id: AtomicU64::new(0),
        })));
        let start = NonNull::from(Box::leak(Box::new(Start {
            header: ThreadStart { entry: worker },
            shared,
        })))
        .cast::<ThreadStart>();
        // SAFETY: `start` is a leaked `Start` (`repr(C)`, header first) that `worker` takes
        // ownership of; `shared` outlives the thread because `drop` joins it first.
        match unsafe { thread::spawn(0, start) } {
            Ok(handle) => Ok(Self {
                shared,
                worker: Locked::new(Some(handle)),
                lost: AtomicBool::new(false),
            }),
            Err(error) => {
                // SAFETY: no thread was created, so both boxes are still ours.
                unsafe {
                    drop(Box::from_raw(start.cast::<Start>().as_ptr()));
                    drop(Box::from_raw(shared.as_ptr()));
                }
                Err(error)
            }
        }
    }

    /// Moves the sink to the heap for good, to register it.
    pub fn leak(self) -> &'static Self {
        Box::leak(Box::new(self))
    }

    fn shared(&self) -> &Shared {
        // SAFETY: `Shared` lives until `drop`.
        unsafe { self.shared.as_ref() }
    }

    fn on_worker(&self) -> bool {
        self.shared().worker_id.load(Ordering::Relaxed) == thread::current_id()
    }

    /// Records dropped because the ring was full.
    pub fn dropped(&self) -> u64 {
        self.shared().dropped.load(Ordering::Relaxed)
    }

    /// Writes out what is queued, then stops and joins the worker. Later records are written on
    /// the logging thread. Does nothing on the worker itself.
    pub fn stop(&self) {
        if self.on_worker() {
            return;
        }
        let shared = self.shared();
        // Under the lock, so every push either precedes it (and is drained) or bypasses the ring.
        shared.queue.with(|queue| queue.stopped = true);
        shared.wake.fetch_add(1, Ordering::SeqCst);
        futex::wake_one(&shared.wake);
        if let Some(handle) = self.worker.with(Option::take)
            && handle.join().is_err()
        {
            self.lost.store(true, Ordering::Relaxed);
        }
    }
}

impl Sink for AsyncSink {
    fn log(&self, record: &Record<'_>) {
        let shared = self.shared();
        let line = format_line(record);
        let text = line.as_str().as_bytes();
        let level = record.level();
        let pushed = shared.queue.with(|queue| {
            if queue.stopped {
                return None;
            }
            let size = (HEADER + text.len()) as u64;
            if queue.head - queue.tail + size > queue.bytes.len() as u64 {
                return Some(false);
            }
            let len = (text.len() as u16).to_le_bytes();
            copy_in(&mut queue.bytes, queue.head, &[level as u8, len[0], len[1]]);
            copy_in(&mut queue.bytes, queue.head + HEADER as u64, text);
            queue.head += size;
            queue.pushed += 1;
            Some(true)
        });
        match pushed {
            Some(true) => shared.wake_worker(),
            Some(false) => {
                shared.dropped.fetch_add(1, Ordering::Relaxed);
            }
            None => shared.out.write_line(level, line.as_str()),
        }
    }

    /// Waits until the worker has written everything logged before the call, then flushes the
    /// destination. On the worker itself it only flushes the destination.
    fn flush(&self) {
        let shared = self.shared();
        if !self.on_worker() {
            let target = shared.queue.with(|queue| queue.pushed);
            shared.flushers.fetch_add(1, Ordering::SeqCst);
            loop {
                let seen = shared.progress.load(Ordering::SeqCst);
                // The worker bumps `progress` after raising `written` whenever it sees a flusher,
                // so a write after this check makes the wait return at once.
                if shared.written.load(Ordering::SeqCst) >= target {
                    break;
                }
                futex::wait(&shared.progress, seen, None);
            }
            shared.flushers.fetch_sub(1, Ordering::SeqCst);
        }
        shared.out.flush();
    }
}

impl Drop for AsyncSink {
    fn drop(&mut self) {
        self.stop();
        if self.worker.get_mut().is_none() && !*self.lost.get_mut() {
            // SAFETY: the worker was joined, so nothing else refers to `Shared`.
            drop(unsafe { Box::from_raw(self.shared.as_ptr()) });
        }
    }
}
