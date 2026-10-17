//! Files over Win32 handles; queues over IoRing, or an IO completion port where IoRing is missing.

mod iocp;
mod ioring;

use core::ptr::{self, NonNull};
use core::sync::atomic::AtomicUsize;
use core::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_HANDLE_EOF, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, GetLastError,
    HANDLE, INVALID_HANDLE_VALUE, WIN32_ERROR,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_ALWAYS, CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_END_OF_FILE_INFO,
    FILE_FLAG_NO_BUFFERING, FILE_FLAG_OVERLAPPED, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_STORAGE_INFO, FileEndOfFileInfo, FileStorageInfo,
    GetFileInformationByHandleEx, GetFileSizeEx, OPEN_ALWAYS, OPEN_EXISTING, ReadFile,
    SetFileInformationByHandle, WriteFile,
};
use windows_sys::Win32::System::IO::{
    GetOverlappedResult, OVERLAPPED, OVERLAPPED_0, OVERLAPPED_0_0,
};
use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE};
use windows_sys::core::HRESULT;

use super::error::{check, from_win32, last_error};
use super::{Backend, with_wide_path};
use crate::contract;
use crate::file::{Completion, Disposition, Engine, Op, OpKind, OpenOptions};
use crate::{Error, ErrorKind, Result};

pub(crate) struct File {
    pub(crate) handle: HANDLE,
    alignment: u32,
    /// Id of the completion port the handle is bound to, or 0. Binding is for the handle's life.
    port: AtomicUsize,
}

// SAFETY: file handles may be used from any thread.
unsafe impl Send for File {}
// SAFETY: as above; every operation on a shared handle is positional.
unsafe impl Sync for File {}

impl Drop for File {
    fn drop(&mut self) {
        // SAFETY: we own the handle and do not use it again.
        unsafe { CloseHandle(self.handle) };
    }
}

#[repr(C)]
pub(crate) struct Request {
    /// First, so the OVERLAPPED pointer a completion carries is the request pointer.
    ov: OVERLAPPED,
    file: HANDLE,
    tag: u64,
    /// Error of an IOCP operation that failed before reaching the port.
    error: WIN32_ERROR,
}

pub(crate) enum Queue {
    IoRing(ioring::Ring),
    Iocp(iocp::Port),
}

// SAFETY: both engines report each pushed operation exactly once and drop their reference to it
// when they do.
unsafe impl contract::File for Backend {
    type File = File;
    type Queue = Queue;
    type Request = Request;
    const REQUEST_INIT: Request = Request {
        ov: OVERLAPPED {
            Internal: 0,
            InternalHigh: 0,
            Anonymous: OVERLAPPED_0 {
                Pointer: ptr::null_mut(),
            },
            hEvent: ptr::null_mut(),
        },
        file: ptr::null_mut(),
        tag: 0,
        error: 0,
    };

    fn open(path: &str, options: &OpenOptions) -> Result<File> {
        let access = if options.write {
            GENERIC_READ | GENERIC_WRITE
        } else {
            GENERIC_READ
        };
        let disposition = match options.disposition {
            Disposition::OpenExisting => OPEN_EXISTING,
            Disposition::OpenOrCreate => OPEN_ALWAYS,
            Disposition::CreateNew => CREATE_NEW,
            Disposition::CreateOrTruncate => CREATE_ALWAYS,
        };
        // Always overlapped: queues need it, and the sync calls cope with it.
        let mut flags = FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED;
        if options.unbuffered {
            flags |= FILE_FLAG_NO_BUFFERING;
        }
        let handle = with_wide_path(path, "", |path| {
            // SAFETY: `path` is NUL-terminated; null security attributes and template are allowed.
            let handle = unsafe {
                CreateFileW(
                    path,
                    access,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    ptr::null(),
                    disposition,
                    flags,
                    ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                Err(last_error())
            } else {
                Ok(handle)
            }
        })?;
        let alignment = if options.unbuffered {
            sector_size(handle)
        } else {
            1
        };
        Ok(File {
            handle,
            alignment,
            port: AtomicUsize::new(0),
        })
    }

    fn alignment(file: &File) -> usize {
        file.alignment as usize
    }

    fn size(file: &File) -> Result<u64> {
        let mut size = 0;
        // SAFETY: `size` is a valid out pointer.
        check(unsafe { GetFileSizeEx(file.handle, &mut size) })?;
        Ok(size as u64)
    }

    fn set_len(file: &File, len: u64) -> Result<()> {
        let info = FILE_END_OF_FILE_INFO {
            EndOfFile: i64::try_from(len).map_err(|_| Error::from_kind(ErrorKind::InvalidInput))?,
        };
        // SAFETY: `info` is a valid FILE_END_OF_FILE_INFO of the given size.
        check(unsafe {
            SetFileInformationByHandle(
                file.handle,
                FileEndOfFileInfo,
                (&raw const info).cast(),
                size_of::<FILE_END_OF_FILE_INFO>() as u32,
            )
        })
    }

    fn read_at(file: &File, buf: &mut [u8], offset: u64) -> Result<usize> {
        // SAFETY: `buf` is valid for writes of its length.
        unsafe { sync_io(file, OpKind::Read, buf.as_mut_ptr(), buf.len(), offset) }
    }

    fn write_at(file: &File, buf: &[u8], offset: u64) -> Result<usize> {
        // SAFETY: `buf` is valid for reads of its length, and WriteFile only reads it.
        unsafe {
            sync_io(
                file,
                OpKind::Write,
                buf.as_ptr().cast_mut(),
                buf.len(),
                offset,
            )
        }
    }

    fn queue_new(capacity: u32, fallback: bool) -> Result<Queue> {
        if !fallback && let Some(ring) = ioring::Ring::new(capacity) {
            return Ok(Queue::IoRing(ring));
        }
        iocp::Port::new().map(Queue::Iocp)
    }

    fn queue_engine(queue: &Queue) -> Engine {
        match queue {
            Queue::IoRing(_) => Engine::IoRing,
            Queue::Iocp(_) => Engine::Iocp,
        }
    }

    unsafe fn queue_push(queue: &mut Queue, request: NonNull<Request>, op: &Op<'_>) -> Result<()> {
        // SAFETY: forwarded from the caller; the request is not in flight, so it is ours to write.
        unsafe {
            match queue {
                Queue::IoRing(ring) => {
                    // A cancel request names the file as well as the operation.
                    (*request.as_ptr()).file = op.file.handle;
                    ring.push(op)
                }
                Queue::Iocp(port) => port.push(request, op),
            }
        }
    }

    unsafe fn queue_cancel(queue: &mut Queue, request: NonNull<Request>, tag: u64) -> Result<()> {
        // SAFETY: the caller guarantees the request is in flight on this queue, so `file` was set
        // by `queue_push` and the handle is still open.
        let file = unsafe { (*request.as_ptr()).file };
        match queue {
            Queue::IoRing(ring) => ring.cancel(file, tag),
            // SAFETY: as above; CancelIoEx only matches the OVERLAPPED against operations in
            // flight on the handle.
            Queue::Iocp(_) => unsafe { iocp::cancel(file, request) },
        }
    }

    fn queue_submit(queue: &mut Queue) -> Result<()> {
        match queue {
            Queue::IoRing(ring) => ring.submit(),
            // Operations start when pushed.
            Queue::Iocp(_) => Ok(()),
        }
    }

    fn queue_poll(
        queue: &mut Queue,
        out: &mut [Completion],
        timeout: Option<Duration>,
    ) -> Result<usize> {
        match queue {
            Queue::IoRing(ring) => ring.poll(out, timeout),
            Queue::Iocp(port) => port.poll(out, timeout),
        }
    }
}

/// An OVERLAPPED for a positional operation at `offset`.
fn overlapped_at(offset: u64) -> OVERLAPPED {
    OVERLAPPED {
        Anonymous: OVERLAPPED_0 {
            Anonymous: OVERLAPPED_0_0 {
                Offset: offset as u32,
                OffsetHigh: (offset >> 32) as u32,
            },
        },
        ..REQUEST_OV_INIT
    }
}

const REQUEST_OV_INIT: OVERLAPPED = <Backend as contract::File>::REQUEST_INIT.ov;

/// The result of a finished operation that failed with `error`. Reading at or past the end of the
/// file is not an error on any other platform, so it is not one here either.
fn io_result(error: WIN32_ERROR) -> Result<usize> {
    if error == ERROR_HANDLE_EOF {
        Ok(0)
    } else {
        Err(from_win32(error))
    }
}

/// HRESULTs from IoRing wrap Win32 errors (facility 7) for everything file-related.
fn hresult_error(hr: HRESULT) -> Error {
    let hr = hr as u32;
    if hr & 0xFFFF_0000 == 0x8007_0000 {
        from_win32(hr & 0xFFFF)
    } else {
        Error::new(ErrorKind::Other, hr as i32)
    }
}

fn hresult_result(hr: HRESULT, bytes: usize) -> Result<usize> {
    if hr >= 0 {
        Ok(bytes)
    } else if hr as u32 == 0x8007_0000 | ERROR_HANDLE_EOF {
        Ok(0)
    } else {
        Err(hresult_error(hr))
    }
}

/// Milliseconds for a Win32 wait, rounded up so short timeouts do not become non-blocking.
fn timeout_ms(timeout: Option<Duration>) -> u32 {
    timeout.map_or(INFINITE, |t| {
        t.as_nanos()
            .div_ceil(1_000_000)
            .min(u128::from(INFINITE - 1)) as u32
    })
}

/// A sync read or write on an overlapped handle.
///
/// # Safety
/// `buf` is valid for `len` bytes of the access `kind` needs.
unsafe fn sync_io(
    file: &File,
    kind: OpKind,
    buf: *mut u8,
    len: usize,
    offset: u64,
) -> Result<usize> {
    // Overlapped handles need an event to wait on. Waiting on the handle itself would race with
    // queued operations on the same file.
    // SAFETY: manual-reset, unsignaled, unnamed; null attributes are allowed.
    let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    if event.is_null() {
        return Err(last_error());
    }
    let mut ov = overlapped_at(offset);
    // The low bit keeps the completion off the port, if the handle is bound to one.
    ov.hEvent = event.map_addr(|addr| addr | 1);
    let len = len as u32;
    // SAFETY: the caller guarantees `buf`; `ov` outlives the operation because we wait for it below.
    let ok = unsafe {
        match kind {
            OpKind::Read => ReadFile(file.handle, buf, len, ptr::null_mut(), &mut ov),
            OpKind::Write => WriteFile(file.handle, buf, len, ptr::null_mut(), &mut ov),
        }
    };
    let error = if ok == 0 {
        // SAFETY: no preconditions.
        unsafe { GetLastError() }
    } else {
        0
    };
    let result = if ok == 0 && error != ERROR_IO_PENDING {
        io_result(error)
    } else {
        let mut n = 0;
        // SAFETY: `ov` belongs to an operation started on this handle; waiting for it ends the
        // operation before `ov` and `buf` are released. The kernel ignores the event's low bit.
        if unsafe { GetOverlappedResult(file.handle, &ov, &mut n, 1) } != 0 {
            Ok(n as usize)
        } else {
            // SAFETY: no preconditions.
            io_result(unsafe { GetLastError() })
        }
    };
    // SAFETY: we own the event and do not use it again.
    unsafe { CloseHandle(event) };
    result
}

/// The alignment unbuffered IO needs: the logical sector size, raised to the physical one so
/// reads never straddle a physical sector (512e drives).
fn sector_size(handle: HANDLE) -> u32 {
    let mut info = FILE_STORAGE_INFO::default();
    // SAFETY: `info` is a valid out buffer of the given size.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileStorageInfo,
            (&raw mut info).cast(),
            size_of::<FILE_STORAGE_INFO>() as u32,
        )
    } != 0;
    let size = if ok {
        info.LogicalBytesPerSector
            .max(info.PhysicalBytesPerSectorForPerformance)
    } else {
        0
    };
    if size.is_power_of_two() { size } else { 4096 }
}
