//! Linux file queues: io_uring, or the shared blocking engine where io_uring is missing or
//! disabled (seccomp in containers, `kernel.io_uring_disabled`).

mod uring;

use core::ptr::NonNull;
use core::time::Duration;

use crate::Result;
use crate::backend::unix::Backend;
use crate::backend::unix::file::{self as posix, Blocking, File, Request};
use crate::contract;
use crate::file::{Completion, Engine, Op, OpenOptions};

pub(crate) enum Queue {
    Uring(uring::Uring),
    Blocking(Blocking),
}

// SAFETY: both engines report each pushed operation exactly once and drop their reference to it
// when they do.
unsafe impl contract::File for Backend {
    type File = File;
    type Queue = Queue;
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

    fn queue_new(capacity: u32, fallback: bool) -> Result<Queue> {
        if !fallback && let Some(ring) = uring::Uring::new(capacity)? {
            return Ok(Queue::Uring(ring));
        }
        Ok(Queue::Blocking(Blocking::new()))
    }

    fn queue_engine(queue: &Queue) -> Engine {
        match queue {
            Queue::Uring(_) => Engine::IoUring,
            Queue::Blocking(_) => Engine::Blocking,
        }
    }

    unsafe fn queue_push(queue: &mut Queue, request: NonNull<Request>, op: &Op<'_>) -> Result<()> {
        // SAFETY: forwarded from the caller.
        unsafe {
            match queue {
                Queue::Uring(ring) => ring.push(op),
                Queue::Blocking(blocking) => {
                    blocking.push(request, op);
                    Ok(())
                }
            }
        }
    }

    unsafe fn queue_cancel(queue: &mut Queue, request: NonNull<Request>, tag: u64) -> Result<()> {
        match queue {
            Queue::Uring(ring) => ring.cancel(tag),
            // SAFETY: forwarded from the caller.
            Queue::Blocking(blocking) => unsafe {
                blocking.cancel(request);
                Ok(())
            },
        }
    }

    fn queue_submit(queue: &mut Queue) -> Result<()> {
        match queue {
            Queue::Uring(ring) => ring.submit(),
            Queue::Blocking(blocking) => {
                blocking.submit();
                Ok(())
            }
        }
    }

    fn queue_poll(
        queue: &mut Queue,
        out: &mut [Completion],
        timeout: Option<Duration>,
    ) -> Result<usize> {
        match queue {
            Queue::Uring(ring) => ring.poll(out, timeout),
            Queue::Blocking(blocking) => Ok(blocking.poll(out)),
        }
    }
}
