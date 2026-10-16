//! POSIX files and the blocking queue engine shared by Linux (as the io_uring fallback) and macOS.

use core::ffi::{c_char, c_int};
use core::ptr::{self, NonNull};

use super::{Backend, errno, from_errno, last_error};
use crate::contract::VMem;
use crate::file::{Completion, Disposition, MAX_PATH_LEN, Op, OpKind, OpenOptions};
use crate::{Error, ErrorKind, Result};

pub(crate) struct File {
    pub(crate) fd: c_int,
    alignment: u32,
}

impl Drop for File {
    fn drop(&mut self) {
        // SAFETY: we own the descriptor and do not use it again.
        unsafe { libc::close(self.fd) };
    }
}

pub(crate) fn open(path: &str, options: &OpenOptions) -> Result<File> {
    let mut c_path = [0u8; MAX_PATH_LEN + 1];
    c_path[..path.len()].copy_from_slice(path.as_bytes());

    let mut flags = libc::O_CLOEXEC;
    flags |= if options.write {
        libc::O_RDWR
    } else {
        libc::O_RDONLY
    };
    flags |= match options.disposition {
        Disposition::OpenExisting => 0,
        Disposition::OpenOrCreate => libc::O_CREAT,
        Disposition::CreateNew => libc::O_CREAT | libc::O_EXCL,
        Disposition::CreateOrTruncate => libc::O_CREAT | libc::O_TRUNC,
    };
    #[cfg(target_os = "linux")]
    if options.unbuffered {
        flags |= libc::O_DIRECT;
    }

    let fd = loop {
        // SAFETY: `c_path` is NUL-terminated (the caller rejects interior NULs).
        let fd = unsafe {
            libc::open(
                c_path.as_ptr().cast::<c_char>(),
                flags,
                0o666 as libc::c_uint,
            )
        };
        if fd >= 0 {
            break fd;
        }
        if errno() != libc::EINTR {
            return Err(last_error());
        }
    };
    let mut file = File { fd, alignment: 1 };
    if options.unbuffered {
        // `file` closes the descriptor if this fails.
        file.alignment = direct_io_alignment(fd)?;
    }
    Ok(file)
}

/// Linux: the direct IO alignment from `statx` (6.1+), else the page size, which satisfies every
/// common block device.
#[cfg(target_os = "linux")]
fn direct_io_alignment(fd: c_int) -> Result<u32> {
    const STATX_DIOALIGN: u32 = 0x2000;
    // `struct statx` is 256 bytes; stx_mask is word 0, stx_dio_mem_align and stx_dio_offset_align
    // are words 38 and 39.
    let mut statx = [0u32; 64];
    // SAFETY: `statx` is a writable 256-byte buffer; the empty path with AT_EMPTY_PATH means `fd`.
    let r = unsafe {
        libc::syscall(
            libc::SYS_statx,
            fd,
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            STATX_DIOALIGN,
            statx.as_mut_ptr(),
        )
    };
    let page = Backend::page_size() as u32;
    if r == 0 && statx[0] & STATX_DIOALIGN != 0 && statx[39] != 0 {
        let align = statx[38].max(statx[39]);
        if align.is_power_of_two() {
            return Ok(align);
        }
    }
    Ok(page)
}

/// macOS: F_NOCACHE has no alignment rule, but page-aligned transfers avoid a copy, and reporting
/// the page size keeps callers portable.
#[cfg(target_vendor = "apple")]
fn direct_io_alignment(fd: c_int) -> Result<u32> {
    // SAFETY: `fd` is open.
    if unsafe { libc::fcntl(fd, libc::F_NOCACHE, 1) } == -1 {
        return Err(last_error());
    }
    Ok(Backend::page_size() as u32)
}

pub(crate) fn alignment(file: &File) -> usize {
    file.alignment as usize
}

pub(crate) fn size(file: &File) -> Result<u64> {
    // SAFETY: `stat` is plain data that fstat fills in.
    let mut stat: libc::stat = unsafe { core::mem::zeroed() };
    // SAFETY: `stat` is a valid out pointer.
    if unsafe { libc::fstat(file.fd, &mut stat) } != 0 {
        return Err(last_error());
    }
    Ok(stat.st_size as u64)
}

pub(crate) fn set_len(file: &File, len: u64) -> Result<()> {
    let len = libc::off_t::try_from(len).map_err(|_| Error::from_kind(ErrorKind::InvalidInput))?;
    loop {
        // SAFETY: no memory is passed.
        if unsafe { libc::ftruncate(file.fd, len) } == 0 {
            return Ok(());
        }
        if errno() != libc::EINTR {
            return Err(last_error());
        }
    }
}

/// One `pread`/`pwrite`, retried on EINTR. Returns bytes transferred or a negative errno, the
/// io_uring convention.
///
/// # Safety
/// `buf` is valid for `len` bytes of the access `kind` needs.
pub(crate) unsafe fn transfer(
    fd: c_int,
    kind: OpKind,
    buf: *mut u8,
    len: usize,
    offset: u64,
) -> isize {
    let Ok(offset) = libc::off_t::try_from(offset) else {
        return -(libc::EINVAL as isize);
    };
    loop {
        // SAFETY: the caller guarantees `buf`.
        let n = unsafe {
            match kind {
                OpKind::Read => libc::pread(fd, buf.cast(), len, offset),
                OpKind::Write => libc::pwrite(fd, buf.cast(), len, offset),
            }
        };
        if n >= 0 {
            return n;
        }
        let code = errno();
        if code != libc::EINTR {
            return -(code as isize);
        }
    }
}

fn transfer_result(n: isize) -> Result<usize> {
    if n >= 0 {
        Ok(n as usize)
    } else {
        Err(from_errno(-n as c_int))
    }
}

pub(crate) fn read_at(file: &File, buf: &mut [u8], offset: u64) -> Result<usize> {
    // SAFETY: `buf` is valid for writes of its length.
    transfer_result(unsafe { transfer(file.fd, OpKind::Read, buf.as_mut_ptr(), buf.len(), offset) })
}

pub(crate) fn write_at(file: &File, buf: &[u8], offset: u64) -> Result<usize> {
    // SAFETY: `buf` is valid for reads of its length, and pwrite only reads it.
    transfer_result(unsafe {
        transfer(
            file.fd,
            OpKind::Write,
            buf.as_ptr().cast_mut(),
            buf.len(),
            offset,
        )
    })
}

/// A queued operation of the blocking engine. io_uring only uses the tag it carries in the SQE.
pub(crate) struct Request {
    next: *mut Request,
    fd: c_int,
    kind: OpKind,
    buf: *mut u8,
    len: u32,
    offset: u64,
    tag: u64,
    /// Bytes transferred or negative errno, once run.
    result: isize,
}

pub(crate) const REQUEST_INIT: Request = Request {
    next: ptr::null_mut(),
    fd: -1,
    kind: OpKind::Read,
    buf: ptr::null_mut(),
    len: 0,
    offset: 0,
    tag: 0,
    result: 0,
};

/// Intrusive FIFO of caller-owned requests.
struct List {
    head: *mut Request,
    tail: *mut Request,
}

impl List {
    const fn new() -> Self {
        Self {
            head: ptr::null_mut(),
            tail: ptr::null_mut(),
        }
    }

    /// # Safety
    /// `request` is valid and in no list.
    unsafe fn push(&mut self, request: *mut Request) {
        // SAFETY: the caller guarantees `request`; `tail` is a live request when non-null.
        unsafe {
            (*request).next = ptr::null_mut();
            if self.tail.is_null() {
                self.head = request;
            } else {
                (*self.tail).next = request;
            }
        }
        self.tail = request;
    }

    fn pop(&mut self) -> Option<*mut Request> {
        let request = self.head;
        if request.is_null() {
            return None;
        }
        // SAFETY: list members stay valid until popped (the queue's contract with its caller).
        self.head = unsafe { (*request).next };
        if self.head.is_null() {
            self.tail = ptr::null_mut();
        }
        Some(request)
    }
}

/// Runs operations with `pread`/`pwrite` when submitted; see `crate::file` for the trade-off.
pub(crate) struct Blocking {
    pending: List,
    done: List,
}

// SAFETY: the lists only point at requests the queue's owner handed over; they move with it.
unsafe impl Send for Blocking {}

impl Blocking {
    pub(crate) const fn new() -> Self {
        Self {
            pending: List::new(),
            done: List::new(),
        }
    }

    /// # Safety
    /// `request`, the buffer and the file stay valid and unmoved until the completion is polled.
    pub(crate) unsafe fn push(&mut self, request: NonNull<Request>, op: &Op<'_>) {
        let request = request.as_ptr();
        // SAFETY: the caller hands us the request; it is not in flight.
        unsafe {
            request.write(Request {
                next: ptr::null_mut(),
                fd: op.file.fd,
                kind: op.kind,
                buf: op.buf,
                len: op.len,
                offset: op.offset,
                tag: op.tag,
                result: 0,
            });
            self.pending.push(request);
        }
    }

    pub(crate) fn submit(&mut self) {
        while let Some(request) = self.pending.pop() {
            // SAFETY: pending requests, their buffers and files are valid (push's contract).
            unsafe {
                let r = &mut *request;
                r.result = transfer(r.fd, r.kind, r.buf, r.len as usize, r.offset);
                self.done.push(request);
            }
        }
    }

    /// Never waits: everything submitted has already finished.
    pub(crate) fn poll(&mut self, out: &mut [Completion]) -> usize {
        self.submit();
        let mut n = 0;
        while n < out.len() {
            let Some(request) = self.done.pop() else {
                break;
            };
            // SAFETY: as above; this is the last time the queue touches the request.
            let request = unsafe { &*request };
            out[n] = Completion {
                tag: request.tag,
                result: transfer_result(request.result),
            };
            n += 1;
        }
        n
    }
}
