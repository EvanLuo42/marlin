//! io_uring over raw syscalls (no liburing).
//!
//! Needs Linux 5.11: `IORING_OP_READ`/`WRITE` (5.6) and `IORING_ENTER_EXT_ARG` (5.11) for waits
//! with a timeout. Older kernels get the blocking engine instead.
//!
//! The rings are shared with the kernel: we own the SQ tail and the CQ head, the kernel owns the
//! SQ head and the CQ tail. Our stores to the indices we own are releases and our loads of the
//! kernel's are acquires, which orders the entries themselves.

use core::ffi::{c_int, c_long, c_uint, c_void};
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;

use crate::backend::unix::{errno, from_errno, last_error};
use crate::file::{Completion, Deadline, Op, OpKind};
use crate::{Error, ErrorKind, Result};

const IORING_OFF_SQ_RING: i64 = 0;
const IORING_OFF_CQ_RING: i64 = 0x0800_0000;
const IORING_OFF_SQES: i64 = 0x1000_0000;

const IORING_FEAT_SINGLE_MMAP: u32 = 1 << 0;
const IORING_FEAT_RW_CUR_POS: u32 = 1 << 3; // 5.6, with IORING_OP_READ/WRITE
const IORING_FEAT_EXT_ARG: u32 = 1 << 8; // 5.11

const IORING_ENTER_GETEVENTS: c_uint = 1 << 0;
const IORING_ENTER_EXT_ARG: c_uint = 1 << 3;

const IORING_OP_READ: u8 = 22;
const IORING_OP_WRITE: u8 = 23;

/// Not in every libc target module; the same on every Linux architecture we build for.
const ETIME: c_int = 62;

#[repr(C)]
#[derive(Default)]
struct SqRingOffsets {
    head: u32,
    tail: u32,
    ring_mask: u32,
    ring_entries: u32,
    flags: u32,
    dropped: u32,
    array: u32,
    resv1: u32,
    user_addr: u64,
}

#[repr(C)]
#[derive(Default)]
struct CqRingOffsets {
    head: u32,
    tail: u32,
    ring_mask: u32,
    ring_entries: u32,
    overflow: u32,
    cqes: u32,
    flags: u32,
    resv1: u32,
    user_addr: u64,
}

#[repr(C)]
#[derive(Default)]
struct Params {
    sq_entries: u32,
    cq_entries: u32,
    flags: u32,
    sq_thread_cpu: u32,
    sq_thread_idle: u32,
    features: u32,
    wq_fd: u32,
    resv: [u32; 3],
    sq_off: SqRingOffsets,
    cq_off: CqRingOffsets,
}

#[repr(C)]
struct Sqe {
    opcode: u8,
    flags: u8,
    ioprio: u16,
    fd: i32,
    off: u64,
    addr: u64,
    len: u32,
    rw_flags: u32,
    user_data: u64,
    buf_index: u16,
    personality: u16,
    splice_fd_in: i32,
    addr3: u64,
    pad: u64,
}

#[repr(C)]
struct Cqe {
    user_data: u64,
    res: i32,
    flags: u32,
}

#[repr(C)]
struct KernelTimespec {
    tv_sec: i64,
    tv_nsec: i64,
}

#[repr(C)]
struct GeteventsArg {
    sigmask: u64,
    sigmask_sz: u32,
    min_wait_usec: u32,
    ts: u64,
}

const _: () = {
    assert!(size_of::<Params>() == 120);
    assert!(size_of::<Sqe>() == 64);
    assert!(size_of::<Cqe>() == 16);
    assert!(size_of::<GeteventsArg>() == 24);
};

/// A mapping of the ring fd.
struct Mapping {
    ptr: *mut u8,
    len: usize,
}

impl Mapping {
    fn new(fd: c_int, len: usize, offset: i64) -> Result<Self> {
        // SAFETY: mapping the ring fd at a kernel-chosen address has no memory preconditions.
        let ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_POPULATE,
                fd,
                offset,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(last_error());
        }
        Ok(Self {
            ptr: ptr.cast(),
            len,
        })
    }

    /// # Safety
    /// `offset` lies inside the mapping and is suitably aligned for `T`.
    unsafe fn at<T>(&self, offset: u32) -> *mut T {
        // SAFETY: forwarded from the caller.
        unsafe { self.ptr.add(offset as usize).cast() }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: we own the mapping.
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

struct RingFd(c_int);

impl Drop for RingFd {
    fn drop(&mut self) {
        // SAFETY: we own the descriptor.
        unsafe { libc::close(self.0) };
    }
}

pub(crate) struct Uring {
    // Field order is drop order: unmap before closing the fd.
    _sq_ring: Mapping,
    /// `None` with IORING_FEAT_SINGLE_MMAP, where the CQ ring shares `sq_ring`.
    _cq_ring: Option<Mapping>,
    _sqes_map: Mapping,
    fd: RingFd,

    sq_head: *const AtomicU32,
    sq_tail: *const AtomicU32,
    sq_mask: u32,
    sq_entries: u32,
    sq_array: *mut u32,
    sqes: *mut Sqe,
    cq_head: *const AtomicU32,
    cq_tail: *const AtomicU32,
    cq_mask: u32,
    cqes: *const Cqe,

    /// Our SQ tail; published to the kernel on each push.
    local_tail: u32,
    /// Entries pushed but not yet consumed by `io_uring_enter`.
    to_submit: u32,
}

// SAFETY: the ring is owned by one thread at a time (`&mut self`), and the kernel side does not
// care which thread enters it.
unsafe impl Send for Uring {}

impl Uring {
    /// `Ok(None)` when io_uring is missing, disabled or too old; the caller falls back.
    pub(super) fn new(capacity: u32) -> Result<Option<Self>> {
        let mut params = Params::default();
        // SAFETY: `params` is a valid io_uring_params.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_io_uring_setup,
                capacity as c_uint,
                &raw mut params,
            )
        };
        if fd < 0 {
            return match errno() {
                libc::ENOSYS | libc::EPERM | libc::EACCES => Ok(None),
                code => Err(from_errno(code)),
            };
        }
        let fd = RingFd(fd as c_int);
        let required = IORING_FEAT_RW_CUR_POS | IORING_FEAT_EXT_ARG;
        if params.features & required != required {
            return Ok(None);
        }

        let sq_len = params.sq_off.array as usize + params.sq_entries as usize * size_of::<u32>();
        let cq_len = params.cq_off.cqes as usize + params.cq_entries as usize * size_of::<Cqe>();
        let single = params.features & IORING_FEAT_SINGLE_MMAP != 0;
        let sq_ring = Mapping::new(
            fd.0,
            if single { sq_len.max(cq_len) } else { sq_len },
            IORING_OFF_SQ_RING,
        )?;
        let cq_ring = if single {
            None
        } else {
            Some(Mapping::new(fd.0, cq_len, IORING_OFF_CQ_RING)?)
        };
        let sqes_map = Mapping::new(
            fd.0,
            params.sq_entries as usize * size_of::<Sqe>(),
            IORING_OFF_SQES,
        )?;

        let sq = &params.sq_off;
        let cq = &params.cq_off;
        let cq_map = cq_ring.as_ref().unwrap_or(&sq_ring);
        // SAFETY: the kernel reported these offsets for the mappings just made; each points at a
        // naturally aligned field of the documented type.
        let ring = unsafe {
            Self {
                sq_head: sq_ring.at(sq.head),
                sq_tail: sq_ring.at(sq.tail),
                sq_mask: *sq_ring.at::<u32>(sq.ring_mask),
                sq_entries: *sq_ring.at::<u32>(sq.ring_entries),
                sq_array: sq_ring.at(sq.array),
                sqes: sqes_map.ptr.cast(),
                cq_head: cq_map.at(cq.head),
                cq_tail: cq_map.at(cq.tail),
                cq_mask: *cq_map.at::<u32>(cq.ring_mask),
                cqes: cq_map.at(cq.cqes),
                local_tail: (*sq_ring.at::<AtomicU32>(sq.tail)).load(Ordering::Relaxed),
                to_submit: 0,
                _sq_ring: sq_ring,
                _cq_ring: cq_ring,
                _sqes_map: sqes_map,
                fd,
            }
        };
        Ok(Some(ring))
    }

    /// Writes an SQE and publishes it; `submit` hands it to the kernel.
    ///
    /// # Safety
    /// The buffer and the file stay valid until the completion is polled.
    pub(super) unsafe fn push(&mut self, op: &Op<'_>) -> Result<()> {
        // SAFETY: `sq_head` points into the live SQ ring.
        let head = unsafe { (*self.sq_head).load(Ordering::Acquire) };
        if self.local_tail.wrapping_sub(head) >= self.sq_entries {
            return Err(Error::from_kind(ErrorKind::WouldBlock));
        }
        let index = self.local_tail & self.sq_mask;
        // SAFETY: `index` is inside the SQE array and the SQ index array, and the kernel does not
        // read that slot until we publish the new tail below.
        unsafe {
            self.sqes.add(index as usize).write(Sqe {
                opcode: match op.kind {
                    OpKind::Read => IORING_OP_READ,
                    OpKind::Write => IORING_OP_WRITE,
                },
                flags: 0,
                ioprio: 0,
                fd: op.file.fd,
                off: op.offset,
                addr: op.buf as u64,
                len: op.len,
                rw_flags: 0,
                user_data: op.tag,
                buf_index: 0,
                personality: 0,
                splice_fd_in: 0,
                addr3: 0,
                pad: 0,
            });
            *self.sq_array.add(index as usize) = index;
        }
        self.local_tail = self.local_tail.wrapping_add(1);
        // SAFETY: `sq_tail` points into the live SQ ring.
        unsafe { (*self.sq_tail).store(self.local_tail, Ordering::Release) };
        self.to_submit += 1;
        Ok(())
    }

    fn enter(
        &self,
        min_complete: u32,
        flags: c_uint,
        arg: *const c_void,
        arg_size: usize,
    ) -> c_long {
        // SAFETY: the ring fd is live; `arg` is null or points at `arg_size` valid bytes.
        unsafe {
            libc::syscall(
                libc::SYS_io_uring_enter,
                self.fd.0,
                self.to_submit as c_uint,
                min_complete as c_uint,
                flags,
                arg,
                arg_size,
            )
        }
    }

    pub(super) fn submit(&mut self) -> Result<()> {
        while self.to_submit > 0 {
            let n = self.enter(0, 0, ptr::null(), 0);
            if n < 0 {
                match errno() {
                    libc::EINTR => continue,
                    // Out of kernel resources for now; the entries stay queued for `poll`.
                    libc::EAGAIN | libc::EBUSY => return Ok(()),
                    code => return Err(from_errno(code)),
                }
            }
            if n == 0 {
                break;
            }
            self.to_submit -= n as u32;
        }
        Ok(())
    }

    pub(super) fn poll(
        &mut self,
        out: &mut [Completion],
        timeout: Option<Duration>,
    ) -> Result<usize> {
        self.submit()?;
        let deadline = Deadline::after(timeout);
        loop {
            let n = self.reap(out);
            if n > 0 {
                return Ok(n);
            }
            let remaining = deadline.remaining();
            if remaining == Some(Duration::ZERO) {
                return Ok(0);
            }
            self.wait(remaining)?;
        }
    }

    /// Submits anything left and waits for one completion or the timeout. Spurious returns are
    /// fine: `poll` loops.
    fn wait(&mut self, timeout: Option<Duration>) -> Result<()> {
        let ts;
        let arg;
        let n = match timeout {
            None => self.enter(1, IORING_ENTER_GETEVENTS, ptr::null(), 0),
            Some(t) => {
                ts = KernelTimespec {
                    tv_sec: t.as_secs().min(i64::MAX as u64) as i64,
                    tv_nsec: i64::from(t.subsec_nanos()),
                };
                arg = GeteventsArg {
                    sigmask: 0,
                    sigmask_sz: 0,
                    min_wait_usec: 0,
                    ts: (&raw const ts).addr() as u64,
                };
                self.enter(
                    1,
                    IORING_ENTER_GETEVENTS | IORING_ENTER_EXT_ARG,
                    (&raw const arg).cast(),
                    size_of::<GeteventsArg>(),
                )
            }
        };
        if n >= 0 {
            self.to_submit -= (n as u32).min(self.to_submit);
            return Ok(());
        }
        match errno() {
            ETIME | libc::EINTR | libc::EAGAIN | libc::EBUSY => Ok(()),
            code => Err(from_errno(code)),
        }
    }

    fn reap(&mut self, out: &mut [Completion]) -> usize {
        // SAFETY: both point into the live CQ ring; only we write the head.
        let (mut head, tail) = unsafe {
            (
                (*self.cq_head).load(Ordering::Relaxed),
                (*self.cq_tail).load(Ordering::Acquire),
            )
        };
        let mut n = 0;
        while head != tail && n < out.len() {
            // SAFETY: entries between head and tail are published by the kernel (acquire above).
            let cqe = unsafe { &*self.cqes.add((head & self.cq_mask) as usize) };
            out[n] = Completion {
                tag: cqe.user_data,
                result: if cqe.res >= 0 {
                    Ok(cqe.res as usize)
                } else {
                    Err(from_errno(-cqe.res))
                },
            };
            head = head.wrapping_add(1);
            n += 1;
        }
        // SAFETY: as above; the release hands the consumed slots back to the kernel.
        unsafe { (*self.cq_head).store(head, Ordering::Release) };
        n
    }
}
