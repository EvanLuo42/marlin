//! macOS files: POSIX with F_NOCACHE for unbuffered IO. There is no kernel completion queue, so
//! every queue uses the blocking engine; see `crate::file` for the trade-off.

use core::ptr::NonNull;
use core::time::Duration;

use crate::Result;
use crate::backend::unix::Backend;
use crate::backend::unix::file::{self as posix, Blocking, File, Request};
use crate::contract;
use crate::file::{Completion, Engine, Op, OpenOptions};

// SAFETY: the blocking engine reports each pushed operation exactly once, when it hands back the
// request.
unsafe impl contract::File for Backend {
    type File = File;
    type Queue = Blocking;
    type Request = Request;
    const REQUEST_INIT: Request = posix::REQUEST_INIT;

    fn open(path: &str, options: &OpenOptions) -> Result<File> {
        posix::open(path, options)
    }

    fn alignment(file: &File) -> usize {
        posix::alignment(file)
    }

    fn size(file: &File) -> Result<u64> {
        posix::size(file)
    }

    fn set_len(file: &File, len: u64) -> Result<()> {
        posix::set_len(file, len)
    }

    fn read_at(file: &File, buf: &mut [u8], offset: u64) -> Result<usize> {
        posix::read_at(file, buf, offset)
    }

    fn write_at(file: &File, buf: &[u8], offset: u64) -> Result<usize> {
        posix::write_at(file, buf, offset)
    }

    fn queue_new(_capacity: u32, _fallback: bool) -> Result<Blocking> {
        Ok(Blocking::new())
    }

    fn queue_engine(_queue: &Blocking) -> Engine {
        Engine::Blocking
    }

    unsafe fn queue_push(
        queue: &mut Blocking,
        request: NonNull<Request>,
        op: &Op<'_>,
    ) -> Result<()> {
        // SAFETY: forwarded from the caller.
        unsafe { queue.push(request, op) };
        Ok(())
    }

    unsafe fn queue_cancel(
        queue: &mut Blocking,
        request: NonNull<Request>,
        _tag: u64,
    ) -> Result<()> {
        // SAFETY: forwarded from the caller.
        unsafe { queue.cancel(request) };
        Ok(())
    }

    fn queue_submit(queue: &mut Blocking) -> Result<()> {
        queue.submit();
        Ok(())
    }

    fn queue_poll(
        queue: &mut Blocking,
        out: &mut [Completion],
        _timeout: Option<Duration>,
    ) -> Result<usize> {
        Ok(queue.poll(out))
    }
}
