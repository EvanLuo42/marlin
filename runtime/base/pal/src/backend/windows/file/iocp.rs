//! IO completion port with overlapped `ReadFile`/`WriteFile`: the queue engine before Windows 11.

use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_NOT_FOUND, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, GetOverlappedResult, GetQueuedCompletionStatusEx,
    OVERLAPPED_ENTRY, PostQueuedCompletionStatus,
};

use super::{File, Request, io_result, overlapped_at, timeout_ms};
use crate::backend::windows::error::{from_win32, last_error};
use crate::file::{Completion, Op, OpKind};
use crate::{Error, ErrorKind, Result};

/// Completion key of packets the OS posts for finished operations.
const KEY_IO: usize = 0;
/// Completion key of packets `push` posts for operations that failed to start, so every error
/// arrives the same way.
const KEY_FAILED: usize = 1;

/// Port ids start at 1; 0 means "not bound" in `File::port`. Ids rather than handle values,
/// because a closed port's handle value can be reused.
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

pub(crate) struct Port {
    handle: HANDLE,
    id: usize,
}

// SAFETY: completion ports may be used from any thread.
unsafe impl Send for Port {}

impl Port {
    pub(super) fn new() -> Result<Self> {
        // SAFETY: creating a port without a file has no preconditions.
        let handle = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, ptr::null_mut(), 0, 1) };
        if handle.is_null() {
            return Err(last_error());
        }
        Ok(Self {
            handle,
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        })
    }

    /// A handle posts completions to one port for its whole life, so a file used with one IOCP
    /// queue cannot move to another.
    fn bind(&self, file: &File) -> Result<()> {
        match file
            .port
            .compare_exchange(0, self.id, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {
                // SAFETY: both handles are live.
                let port = unsafe { CreateIoCompletionPort(file.handle, self.handle, KEY_IO, 0) };
                if port.is_null() {
                    let error = last_error();
                    file.port.store(0, Ordering::Release);
                    return Err(error);
                }
                Ok(())
            }
            Err(id) if id == self.id => Ok(()),
            Err(_) => Err(Error::from_kind(ErrorKind::InvalidInput)),
        }
    }

    /// # Safety
    /// `request`, the buffer and the file stay valid and unmoved until the completion is polled.
    pub(super) unsafe fn push(&mut self, request: NonNull<Request>, op: &Op<'_>) -> Result<()> {
        self.bind(op.file)?;
        let request = request.as_ptr();
        // SAFETY: the caller hands us the request; it is not in flight.
        unsafe {
            request.write(Request {
                ov: overlapped_at(op.offset),
                file: op.file.handle,
                tag: op.tag,
                error: 0,
            })
        };
        // SAFETY: the caller keeps the buffer and request alive until the completion is polled.
        let ok = unsafe {
            let ov = &raw mut (*request).ov;
            match op.kind {
                OpKind::Read => ReadFile(op.file.handle, op.buf, op.len, ptr::null_mut(), ov),
                OpKind::Write => WriteFile(op.file.handle, op.buf, op.len, ptr::null_mut(), ov),
            }
        };
        if ok == 0 {
            // SAFETY: no preconditions.
            let error = unsafe { GetLastError() };
            if error != ERROR_IO_PENDING {
                // Nothing reaches the port for an operation that failed to start; post it
                // ourselves.
                // SAFETY: as above; the request is not in flight.
                unsafe { (*request).error = error };
                // SAFETY: the port is live; the request stays valid until the packet is polled.
                let posted = unsafe {
                    PostQueuedCompletionStatus(self.handle, 0, KEY_FAILED, &raw const (*request).ov)
                };
                if posted == 0 {
                    return Err(last_error());
                }
            }
        }
        Ok(())
    }

    pub(super) fn poll(
        &mut self,
        out: &mut [Completion],
        timeout: Option<Duration>,
    ) -> Result<usize> {
        const BATCH: usize = 64;
        let mut entries = [OVERLAPPED_ENTRY::default(); BATCH];
        let count = out.len().min(BATCH) as u32;
        let mut removed = 0;
        // SAFETY: `entries` holds at least `count` entries; `removed` is a valid out pointer.
        let ok = unsafe {
            GetQueuedCompletionStatusEx(
                self.handle,
                entries.as_mut_ptr(),
                count,
                &mut removed,
                timeout_ms(timeout),
                0,
            )
        };
        if ok == 0 {
            // SAFETY: no preconditions.
            let error = unsafe { GetLastError() };
            return if error == WAIT_TIMEOUT {
                Ok(0)
            } else {
                Err(from_win32(error))
            };
        }

        for (entry, out) in entries[..removed as usize].iter().zip(out) {
            // SAFETY: every packet on this port carries the OVERLAPPED of a request pushed by
            // `push`, which is its first field. The caller keeps it alive until now, and the OS is
            // done with it.
            let request = unsafe { &*entry.lpOverlapped.cast::<Request>() };
            let result = if entry.lpCompletionKey == KEY_FAILED {
                io_result(request.error)
            } else if request.ov.Internal == 0 {
                // STATUS_SUCCESS
                Ok(entry.dwNumberOfBytesTransferred as usize)
            } else {
                let mut n = 0;
                // SAFETY: the operation has finished; this only converts its NTSTATUS to a Win32
                // error.
                if unsafe { GetOverlappedResult(request.file, &request.ov, &mut n, 0) } != 0 {
                    Ok(n as usize)
                } else {
                    // SAFETY: no preconditions.
                    io_result(unsafe { GetLastError() })
                }
            };
            *out = Completion {
                tag: request.tag,
                result,
            };
        }
        Ok(removed as usize)
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        // SAFETY: we own the handle and do not use it again.
        unsafe { CloseHandle(self.handle) };
    }
}

/// Cancels the operation whose OVERLAPPED is `request`'s. Finished operations are left alone.
///
/// # Safety
/// `file` is the open handle the request was pushed on.
pub(super) unsafe fn cancel(file: HANDLE, request: NonNull<Request>) -> Result<()> {
    // SAFETY: the handle is open; CancelIoEx only compares the pointer with operations in flight.
    let ok = unsafe { CancelIoEx(file, &raw const (*request.as_ptr()).ov) };
    // SAFETY: no preconditions.
    if ok != 0 || unsafe { GetLastError() } == ERROR_NOT_FOUND {
        Ok(())
    } else {
        Err(last_error())
    }
}
