use core::ptr::NonNull;
use core::time::Duration;

use crate::Result;
use crate::file::{Completion, Engine, Op, OpenOptions};

/// # Safety
///
/// A queue must report every pushed operation exactly once, and must not touch its request slot or
/// buffer after reporting it.
pub(crate) unsafe trait File {
    /// An open file; dropping it closes the file.
    type File: Send + Sync;
    /// An IO queue; dropping it releases OS resources. The caller drains it first.
    type Queue: Send;
    /// Per-operation storage the caller owns, pinned from push until its completion is polled.
    type Request;
    const REQUEST_INIT: Self::Request;

    /// `path` is non-empty, at most `MAX_PATH_LEN` bytes and has no NUL (checked by the caller).
    fn open(path: &str, options: &OpenOptions) -> Result<Self::File>;
    /// Alignment of buffers, offsets and lengths: 1 unless the file was opened unbuffered.
    fn alignment(file: &Self::File) -> usize;
    fn size(file: &Self::File) -> Result<u64>;
    fn set_len(file: &Self::File, len: u64) -> Result<()>;
    /// `buf.len() <= MAX_IO_LEN`. Reading at or past the end returns 0.
    fn read_at(file: &Self::File, buf: &mut [u8], offset: u64) -> Result<usize>;
    /// `buf.len() <= MAX_IO_LEN`.
    fn write_at(file: &Self::File, buf: &[u8], offset: u64) -> Result<usize>;

    /// `capacity` is in `1..=MAX_QUEUE_CAPACITY`. `fallback` asks for the portable engine.
    fn queue_new(capacity: u32, fallback: bool) -> Result<Self::Queue>;
    fn queue_engine(queue: &Self::Queue) -> Engine;
    /// Prepares `op`; the engine may also start it. Errors here mean the operation was not queued;
    /// IO errors are reported through its completion instead.
    ///
    /// # Safety
    /// Fewer than `capacity` operations are in flight. `request`, the buffer and the file stay valid
    /// and unmoved until the completion is polled.
    unsafe fn queue_push(
        queue: &mut Self::Queue,
        request: NonNull<Self::Request>,
        op: &Op<'_>,
    ) -> Result<()>;
    /// Hands prepared operations to the OS.
    fn queue_submit(queue: &mut Self::Queue) -> Result<()>;
    /// Submits, then writes up to `out.len()` (at least 1) completions, waiting up to `timeout`
    /// (`None` = forever) for the first. At least one operation is in flight.
    fn queue_poll(
        queue: &mut Self::Queue,
        out: &mut [Completion],
        timeout: Option<Duration>,
    ) -> Result<usize>;
}
