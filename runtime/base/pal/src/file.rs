//! Files and completion-based asynchronous IO.
//!
//! [`File`] opens, sizes and synchronously reads or writes files at offsets. [`Queue`] is the
//! asynchronous core: push reads and writes at offsets into caller-owned buffers, [`submit`] them,
//! then [`poll`] for [`Completion`]s identified by a caller-chosen tag. Nothing allocates: the
//! caller provides a [`Request`] slot per operation and the completion array.
//!
//! | Platform | [`Engine`] | Fallback ([`Queue::new_fallback`], or when the first is unavailable) |
//! |---|---|---|
//! | Windows | `IoRing` (Windows 11 22H2+) | `Iocp`: overlapped `ReadFile`/`WriteFile` on a completion port |
//! | Linux | `IoUring` (5.11+, raw syscalls) | `Blocking`: `pread`/`pwrite` (io_uring missing or disabled) |
//! | macOS | `Blocking` | — |
//!
//! `Blocking` runs each operation with `pread`/`pwrite` inside [`submit`] (or the [`poll`] that
//! submits), on the calling thread. The API stays the same, but the IO no longer overlaps with
//! the caller's work and requests run one at a time, so the device sees a queue depth of 1. Run
//! such a queue on a dedicated IO thread. A helper thread per queue would hide that, but costs a
//! thread and a handoff per batch on the one platform where it matters less (Apple SSDs are fast
//! at QD1); `dispatch_io` is the candidate if macOS streaming needs more.
//!
//! # Unbuffered IO
//!
//! [`OpenOptions::unbuffered`] bypasses the OS cache (`FILE_FLAG_NO_BUFFERING`, `O_DIRECT`,
//! `F_NOCACHE`). Buffers, offsets and lengths must then be multiples of [`File::alignment`], which
//! is checked here and reported as `InvalidInput`. Reads may still end short at the end of the
//! file. macOS does not require the alignment, but reports the page size so code stays portable.
//!
//! # Paths
//!
//! UTF-8, converted on the stack, at most [`MAX_PATH_LEN`] bytes. On Windows both `/` and `\`
//! separate components, relative paths resolve against the current directory, and the result is
//! passed with a `\\?\` prefix, so the 260-character `MAX_PATH` limit does not apply.
//!
//! [`submit`]: Queue::submit
//! [`poll`]: Queue::poll

use core::cell::UnsafeCell;
use core::marker::PhantomPinned;
use core::ptr::NonNull;
use core::time::Duration;

use crate::backend::imp::Backend;
use crate::contract::File as Contract;
use crate::{Error, ErrorKind, Result};

/// Longest accepted path, in UTF-8 bytes.
pub const MAX_PATH_LEN: usize = 1024;
/// Longest single transfer. Sync calls clamp to it (and may return short); queued operations
/// longer than this are `InvalidInput`. Linux caps every read at this size anyway.
pub const MAX_IO_LEN: usize = 0x7FFF_F000;
/// Most operations a [`Queue`] may have in flight.
pub const MAX_QUEUE_CAPACITY: u32 = 4096;
/// The one tag operations may not use: engines tag their own cancel requests with it.
pub const CANCEL_TAG: u64 = u64::MAX;

/// What to do when the file does or does not exist.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Disposition {
    /// Fail with `NotFound` if missing.
    #[default]
    OpenExisting,
    /// Create if missing.
    OpenOrCreate,
    /// Fail with `AlreadyExists` if present.
    CreateNew,
    /// Create if missing, truncate if present. Needs write access.
    CreateOrTruncate,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct OpenOptions {
    /// Read-write when set, read-only otherwise.
    pub write: bool,
    pub disposition: Disposition,
    /// Bypass the OS cache; see the [module docs](self#unbuffered-io).
    pub unbuffered: bool,
}

impl OpenOptions {
    /// Read-only, the file must exist.
    pub const fn read() -> Self {
        Self {
            write: false,
            disposition: Disposition::OpenExisting,
            unbuffered: false,
        }
    }

    /// Read-write, the file must exist.
    pub const fn read_write() -> Self {
        Self {
            write: true,
            ..Self::read()
        }
    }

    pub const fn disposition(self, disposition: Disposition) -> Self {
        Self {
            disposition,
            ..self
        }
    }

    pub const fn unbuffered(self, unbuffered: bool) -> Self {
        Self { unbuffered, ..self }
    }
}

/// An open file. Dropping it closes the file.
///
/// Files are opened shared (others may read, write and delete them) and, on Windows, for
/// overlapped IO, which is what lets one handle serve both the sync calls and a [`Queue`].
pub struct File(<Backend as Contract>::File);

impl File {
    pub fn open(path: &str, options: &OpenOptions) -> Result<Self> {
        let invalid = path.is_empty()
            || path.len() > MAX_PATH_LEN
            || path.as_bytes().contains(&0)
            || (options.disposition == Disposition::CreateOrTruncate && !options.write);
        if invalid {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        Backend::open(path, options).map(Self)
    }

    /// Required alignment of buffer addresses, offsets and lengths: 1 for buffered files, the
    /// sector or direct-IO alignment for unbuffered ones (a power of two).
    #[inline]
    pub fn alignment(&self) -> usize {
        Backend::alignment(&self.0)
    }

    pub fn size(&self) -> Result<u64> {
        Backend::size(&self.0)
    }

    /// Truncates or extends (with zeroes) the file. Needs write access.
    pub fn set_len(&self, len: u64) -> Result<()> {
        Backend::set_len(&self.0, len)
    }

    /// Reads up to `buf.len()` bytes (clamped to [`MAX_IO_LEN`]) at `offset`, without moving any
    /// file position. Returns the bytes read: fewer near the end of the file, 0 at or past it.
    pub fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        let len = buf.len().min(MAX_IO_LEN);
        self.check_aligned(buf.as_ptr(), len, offset)?;
        Backend::read_at(&self.0, &mut buf[..len], offset)
    }

    /// Writes up to `buf.len()` bytes (clamped to [`MAX_IO_LEN`]) at `offset`, extending the file
    /// if needed. Returns the bytes written.
    pub fn write_at(&self, buf: &[u8], offset: u64) -> Result<usize> {
        let len = buf.len().min(MAX_IO_LEN);
        self.check_aligned(buf.as_ptr(), len, offset)?;
        Backend::write_at(&self.0, &buf[..len], offset)
    }

    #[inline]
    pub(crate) fn raw(&self) -> &<Backend as Contract>::File {
        &self.0
    }

    fn check_aligned(&self, buf: *const u8, len: usize, offset: u64) -> Result<()> {
        let mask = self.alignment() - 1;
        if (buf.addr() | len) & mask != 0 || offset & mask as u64 != 0 {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        Ok(())
    }
}

/// Which mechanism a [`Queue`] uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine {
    /// Windows 11 IoRing.
    IoRing,
    /// Windows IO completion port with overlapped IO.
    Iocp,
    /// Linux io_uring.
    IoUring,
    /// `pread`/`pwrite` run inside `submit`; see the [module docs](self).
    Blocking,
}

/// Storage for one queued operation. Owned by the caller and handed to the queue by pointer; it
/// must stay in place and alive until the operation's completion has been polled.
#[repr(transparent)]
pub struct Request {
    inner: UnsafeCell<<Backend as Contract>::Request>,
    _pinned: PhantomPinned,
}

// SAFETY: a request is plain storage; the queue that owns it while in flight is Send.
unsafe impl Send for Request {}
// SAFETY: `Request` has no `&self` methods.
unsafe impl Sync for Request {}

impl Request {
    pub const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(<Backend as Contract>::REQUEST_INIT),
            _pinned: PhantomPinned,
        }
    }
}

impl Default for Request {
    fn default() -> Self {
        Self::new()
    }
}

/// A finished operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Completion {
    /// The tag passed when the operation was pushed.
    pub tag: u64,
    /// Bytes transferred (a read at or past the end of the file is `Ok(0)`), or the OS error.
    pub result: Result<usize>,
}

impl Completion {
    /// Filler for completion arrays.
    pub const EMPTY: Self = Self {
        tag: 0,
        result: Ok(0),
    };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OpKind {
    Read,
    Write,
}

/// One operation, as handed to the backend.
pub(crate) struct Op<'a> {
    pub kind: OpKind,
    pub file: &'a <Backend as Contract>::File,
    pub buf: *mut u8,
    pub len: u32,
    pub offset: u64,
    pub tag: u64,
}

/// A completion-based IO queue. See the [module docs](self).
///
/// Single-owner: every method takes `&mut self`. It is `Send`, so a dedicated IO thread may own it.
/// Dropping the queue waits for every operation in flight.
pub struct Queue {
    raw: <Backend as Contract>::Queue,
    capacity: u32,
    in_flight: u32,
}

impl Queue {
    /// A queue using the platform's best engine, for at most `capacity` operations in flight
    /// (`1..=MAX_QUEUE_CAPACITY`).
    pub fn new(capacity: u32) -> Result<Self> {
        Self::create(capacity, false)
    }

    /// A queue using the platform's fallback engine (see the table in the [module docs](self)).
    /// Mostly for tests and comparisons.
    pub fn new_fallback(capacity: u32) -> Result<Self> {
        Self::create(capacity, true)
    }

    fn create(capacity: u32, fallback: bool) -> Result<Self> {
        if !(1..=MAX_QUEUE_CAPACITY).contains(&capacity) {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        Ok(Self {
            raw: Backend::queue_new(capacity, fallback)?,
            capacity,
            in_flight: 0,
        })
    }

    #[inline]
    pub fn engine(&self) -> Engine {
        Backend::queue_engine(&self.raw)
    }

    #[inline]
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Operations pushed whose completions have not been polled yet.
    #[inline]
    pub fn in_flight(&self) -> u32 {
        self.in_flight
    }

    /// Queues a read of `len` bytes at `offset` into `buf`. It starts at the next [`submit`] or
    /// [`poll`] (IOCP starts it immediately). `WouldBlock` if `capacity` operations are in flight;
    /// `InvalidInput` if `len > MAX_IO_LEN`, `tag` is [`CANCEL_TAG`] or an unbuffered file's
    /// alignment is not met. IO errors arrive as the completion's result.
    ///
    /// # Safety
    ///
    /// Until the completion with this `tag` has been returned by [`poll`] (or the queue is dropped):
    /// `request` must stay valid and unmoved and must not be pushed again, `buf` must stay valid
    /// for `len` bytes of writes and must not be accessed otherwise, and `file` must stay open.
    ///
    /// [`submit`]: Self::submit
    /// [`poll`]: Self::poll
    pub unsafe fn read(
        &mut self,
        request: NonNull<Request>,
        file: &File,
        buf: *mut u8,
        len: usize,
        offset: u64,
        tag: u64,
    ) -> Result<()> {
        // SAFETY: forwarded from the caller.
        unsafe { self.push(OpKind::Read, request, file, buf, len, offset, tag) }
    }

    /// Queues a write of `len` bytes from `buf` at `offset`; otherwise like [`read`](Self::read).
    ///
    /// # Safety
    ///
    /// As for [`read`](Self::read), except that `buf` must stay valid for reads and must not be
    /// written.
    pub unsafe fn write(
        &mut self,
        request: NonNull<Request>,
        file: &File,
        buf: *const u8,
        len: usize,
        offset: u64,
        tag: u64,
    ) -> Result<()> {
        // SAFETY: forwarded from the caller.
        unsafe {
            self.push(
                OpKind::Write,
                request,
                file,
                buf.cast_mut(),
                len,
                offset,
                tag,
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn push(
        &mut self,
        kind: OpKind,
        request: NonNull<Request>,
        file: &File,
        buf: *mut u8,
        len: usize,
        offset: u64,
        tag: u64,
    ) -> Result<()> {
        if self.in_flight >= self.capacity {
            return Err(Error::from_kind(ErrorKind::WouldBlock));
        }
        if len > MAX_IO_LEN || tag == CANCEL_TAG {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        file.check_aligned(buf, len, offset)?;
        let op = Op {
            kind,
            file: &file.0,
            buf,
            len: len as u32,
            offset,
            tag,
        };
        // `Request` is a transparent wrapper around the backend's request.
        let request = request.cast::<<Backend as Contract>::Request>();
        // SAFETY: fewer than `capacity` operations are in flight; the rest is the caller's contract.
        unsafe { Backend::queue_push(&mut self.raw, request, &op) }?;
        self.in_flight += 1;
        Ok(())
    }

    /// Asks for the operation pushed with `request` and `tag` to be stopped. It still completes
    /// exactly once, through [`poll`](Self::poll): with `Cancelled` if it was stopped in time,
    /// otherwise with its normal result (operations that already finished are not undone). Takes
    /// effect at the next submit on IoRing and io_uring, and at once elsewhere.
    ///
    /// `WouldBlock` when `capacity` cancellations are already pending.
    ///
    /// # Safety
    ///
    /// `request` must have been pushed to this queue with `tag`, and its completion must not have
    /// been polled yet.
    pub unsafe fn cancel(&mut self, request: NonNull<Request>, tag: u64) -> Result<()> {
        let request = request.cast::<<Backend as Contract>::Request>();
        // SAFETY: forwarded from the caller.
        unsafe { Backend::queue_cancel(&mut self.raw, request, tag) }
    }

    /// Hands queued operations to the OS without waiting. [`poll`](Self::poll) does this too.
    pub fn submit(&mut self) -> Result<()> {
        Backend::queue_submit(&mut self.raw)
    }

    /// Submits queued operations, then fills `out` with finished ones, waiting up to `timeout`
    /// (`None` = forever, zero = don't wait) for the first. Returns how many were written: 0 on
    /// timeout, and immediately if nothing is in flight or `out` is empty.
    pub fn poll(&mut self, out: &mut [Completion], timeout: Option<Duration>) -> Result<usize> {
        if self.in_flight == 0 || out.is_empty() {
            return Ok(0);
        }
        let n = Backend::queue_poll(&mut self.raw, out, timeout)?;
        debug_assert!(n as u32 <= self.in_flight);
        self.in_flight -= n as u32;
        Ok(n)
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        // The OS may still write into request slots and buffers; they are only guaranteed to
        // outlive the queue, so wait for everything. Failing to wait would be unsound.
        let mut sink = [Completion::EMPTY; 32];
        while self.in_flight > 0 {
            if self.poll(&mut sink, None).is_err() {
                crate::process::abort();
            }
        }
    }
}

/// A point in time for backends that wait in steps. macOS has none.
#[cfg_attr(marlin_platform = "macos", allow(dead_code))]
pub(crate) struct Deadline(Option<u64>);

#[cfg_attr(marlin_platform = "macos", allow(dead_code))]
impl Deadline {
    /// `None` never expires.
    pub(crate) fn after(timeout: Option<Duration>) -> Self {
        Self(timeout.map(|t| {
            let tps = u128::from(crate::time::ticks_per_second());
            let ticks = (t.as_nanos() * tps).div_ceil(1_000_000_000);
            crate::time::ticks().saturating_add(ticks.min(u128::from(u64::MAX)) as u64)
        }))
    }

    /// `None` = wait forever, zero = expired.
    pub(crate) fn remaining(&self) -> Option<Duration> {
        let end = self.0?;
        let left = end.saturating_sub(crate::time::ticks());
        let tps = u128::from(crate::time::ticks_per_second());
        let nanos = (u128::from(left) * 1_000_000_000).div_ceil(tps);
        Some(Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64))
    }
}
