use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;
use core::marker::PhantomData;
use core::mem::ManuallyDrop;
use core::ptr::{self, NonNull};
use core::time::Duration;

use marlin_pal::file as pal;

use crate::{Engine, Error, ErrorKind, File, IoBuffer, Result};

/// Identifies one submitted operation until its [`Completed`] is returned.
///
/// Ids are not reused while their operation is in flight; a slot's next id gets a new generation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RequestId {
    index: u32,
    generation: u32,
}

impl RequestId {
    fn tag(self) -> u64 {
        u64::from(self.generation) << 32 | u64::from(self.index)
    }

    fn from_tag(tag: u64) -> Self {
        Self {
            index: tag as u32,
            generation: (tag >> 32) as u32,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum OpKind {
    Read,
    Write,
}

/// A finished operation, handing back its buffer.
#[derive(Debug)]
pub struct Completed {
    pub id: RequestId,
    pub kind: OpKind,
    pub buffer: IoBuffer,
    /// Bytes transferred: fewer than requested when a read reaches the end of the file, 0 at or
    /// past it. Or the OS error.
    pub result: Result<usize>,
}

/// A submission that was refused (queue full, bad alignment, ...), handing back the buffer.
pub struct SubmitError {
    pub error: Error,
    pub buffer: IoBuffer,
}

impl fmt::Debug for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubmitError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.error, f)
    }
}

impl core::error::Error for SubmitError {}

struct Slot {
    buffer: Option<IoBuffer>,
    kind: OpKind,
    generation: u32,
}

/// Completion-based asynchronous file IO.
///
/// [`read`](Self::read) and [`write`](Self::write) take ownership of an [`IoBuffer`] and return a
/// [`RequestId`]; [`poll`](Self::poll), [`wait`](Self::wait) and
/// [`wait_timeout`](Self::wait_timeout) return [`Completed`] operations with their buffers, in
/// completion order. Submissions are batched: they start at the next poll or wait, or at an
/// explicit [`submit`](Self::submit).
///
/// Files are borrowed for `'f`; dropping the queue waits for every operation in flight. The queue
/// is `Send` but not `Sync`: give it to one IO thread.
///
/// The engine depends on the platform; see [`Engine`]. On macOS (and Linux without io_uring)
/// operations run synchronously inside `submit`/`poll`.
pub struct IoQueue<'f> {
    raw: ManuallyDrop<pal::Queue>,
    /// The OS may write these while operations are in flight, so they are only reached through
    /// raw pointers. Leaked, not freed, if the queue is leaked.
    requests: NonNull<pal::Request>,
    slots: Vec<Slot>,
    free: Vec<u32>,
    /// Completions fetched from the OS but not yet returned.
    staged: Box<[pal::Completion]>,
    staged_next: usize,
    staged_len: usize,
    _files: PhantomData<&'f File>,
}

// SAFETY: the requests are owned by the queue; everything else is Send.
unsafe impl Send for IoQueue<'_> {}

impl<'f> IoQueue<'f> {
    /// A queue on the platform's best engine for up to `capacity` operations in flight
    /// (`1..=MAX_QUEUE_CAPACITY`).
    pub fn new(capacity: u32) -> Result<Self> {
        Self::create(pal::Queue::new(capacity)?)
    }

    /// A queue on the platform's fallback engine (IOCP on Windows, blocking on Linux).
    pub fn with_fallback(capacity: u32) -> Result<Self> {
        Self::create(pal::Queue::new_fallback(capacity)?)
    }

    fn create(raw: pal::Queue) -> Result<Self> {
        let capacity = raw.capacity();
        let requests: Box<[pal::Request]> = (0..capacity).map(|_| pal::Request::new()).collect();
        let requests = NonNull::new(Box::into_raw(requests).cast::<pal::Request>()).unwrap();
        let slots = (0..capacity)
            .map(|_| Slot {
                buffer: None,
                kind: OpKind::Read,
                generation: 0,
            })
            .collect();
        let staged = alloc::vec![pal::Completion::EMPTY; capacity.min(64) as usize];
        Ok(Self {
            raw: ManuallyDrop::new(raw),
            requests,
            slots,
            // Popped from the back, so slot 0 goes first.
            free: (0..capacity).rev().collect(),
            staged: staged.into_boxed_slice(),
            staged_next: 0,
            staged_len: 0,
            _files: PhantomData,
        })
    }

    #[inline]
    pub fn engine(&self) -> Engine {
        self.raw.engine()
    }

    #[inline]
    pub fn capacity(&self) -> u32 {
        self.slots.len() as u32
    }

    /// Operations submitted whose completions have not been returned yet.
    #[inline]
    pub fn in_flight(&self) -> u32 {
        (self.slots.len() - self.free.len()) as u32
    }

    /// Reads `buffer.len()` bytes of `file` at `offset` into `buffer`.
    ///
    /// Fails with `WouldBlock` when `capacity` operations are in flight, and with
    /// `InvalidInput` when an unbuffered file's alignment is not met; see
    /// [`IoBuffer::for_file`]. IO errors arrive in the [`Completed`] result instead.
    pub fn read(
        &mut self,
        file: &'f File,
        buffer: IoBuffer,
        offset: u64,
    ) -> Result<RequestId, SubmitError> {
        self.push(OpKind::Read, file, buffer, offset)
    }

    /// Writes all of `buffer` to `file` at `offset`; otherwise like [`read`](Self::read).
    pub fn write(
        &mut self,
        file: &'f File,
        buffer: IoBuffer,
        offset: u64,
    ) -> Result<RequestId, SubmitError> {
        self.push(OpKind::Write, file, buffer, offset)
    }

    fn push(
        &mut self,
        kind: OpKind,
        file: &'f File,
        mut buffer: IoBuffer,
        offset: u64,
    ) -> Result<RequestId, SubmitError> {
        let Some(index) = self.free.pop() else {
            return Err(SubmitError {
                error: Error::from_kind(ErrorKind::WouldBlock),
                buffer,
            });
        };
        let slot = &mut self.slots[index as usize];
        slot.generation = slot.generation.wrapping_add(1);
        let id = RequestId {
            index,
            generation: slot.generation,
        };
        // SAFETY: `index < capacity`, the length of the request array.
        let request = unsafe { self.requests.add(index as usize) };
        let (ptr, len) = (buffer.as_mut_ptr(), buffer.len());
        // SAFETY: slot `index` was free, so its request is idle; it stays put until the queue is
        // dropped. The buffer's heap memory moves into the slot below and stays there, untouched,
        // until the completion is returned. `file` is borrowed for `'f`, which outlives the queue.
        let pushed = unsafe {
            match kind {
                OpKind::Read => self.raw.read(request, &file.0, ptr, len, offset, id.tag()),
                OpKind::Write => self.raw.write(request, &file.0, ptr, len, offset, id.tag()),
            }
        };
        match pushed {
            Ok(()) => {
                slot.buffer = Some(buffer);
                slot.kind = kind;
                Ok(id)
            }
            Err(error) => {
                self.free.push(index);
                Err(SubmitError { error, buffer })
            }
        }
    }

    /// Starts queued operations without waiting for any.
    pub fn submit(&mut self) -> Result<()> {
        self.raw.submit()
    }

    /// Returns a finished operation if there is one, without waiting.
    pub fn poll(&mut self) -> Result<Option<Completed>> {
        self.next(Some(Duration::ZERO))
    }

    /// Waits for an operation to finish. `None` only when nothing is in flight.
    pub fn wait(&mut self) -> Result<Option<Completed>> {
        self.next(None)
    }

    /// Waits up to `timeout` for an operation to finish. `None` on timeout or when nothing is in
    /// flight.
    pub fn wait_timeout(&mut self, timeout: Duration) -> Result<Option<Completed>> {
        self.next(Some(timeout))
    }

    fn next(&mut self, timeout: Option<Duration>) -> Result<Option<Completed>> {
        if self.staged_next == self.staged_len {
            self.staged_next = 0;
            self.staged_len = 0;
            self.staged_len = self.raw.poll(&mut self.staged, timeout)?;
            if self.staged_len == 0 {
                return Ok(None);
            }
        }
        let completion = self.staged[self.staged_next];
        self.staged_next += 1;

        let id = RequestId::from_tag(completion.tag);
        let slot = &mut self.slots[id.index as usize];
        debug_assert_eq!(slot.generation, id.generation);
        let buffer = slot
            .buffer
            .take()
            .expect("completion for an idle request slot");
        self.free.push(id.index);
        Ok(Some(Completed {
            id,
            kind: slot.kind,
            buffer,
            result: completion.result,
        }))
    }
}

impl Drop for IoQueue<'_> {
    fn drop(&mut self) {
        // SAFETY: `raw` is not used again. Dropping it waits for every operation, after which the
        // OS holds no pointers into the requests or the buffers.
        unsafe { ManuallyDrop::drop(&mut self.raw) };
        let requests = ptr::slice_from_raw_parts_mut(self.requests.as_ptr(), self.slots.len());
        // SAFETY: allocated in `create` as a boxed slice of `capacity` requests; nothing refers to
        // it any more.
        drop(unsafe { Box::from_raw(requests) });
    }
}
