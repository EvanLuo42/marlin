use core::ffi::c_int;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicUsize, Ordering};

use super::{Backend, last_error};
use crate::contract::VMem;
use crate::{Protection, Result};

#[cfg(target_os = "linux")]
const RESERVE_FLAGS: c_int = libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE;
#[cfg(not(target_os = "linux"))]
const RESERVE_FLAGS: c_int = libc::MAP_PRIVATE | libc::MAP_ANON;

// 0 means not queried yet.
static PAGE_SIZE: AtomicUsize = AtomicUsize::new(0);

fn prot_flags(prot: Protection) -> c_int {
    match prot {
        Protection::NoAccess => libc::PROT_NONE,
        Protection::Read => libc::PROT_READ,
        Protection::ReadWrite => libc::PROT_READ | libc::PROT_WRITE,
        Protection::ReadExecute => libc::PROT_READ | libc::PROT_EXEC,
        Protection::ReadWriteExecute => libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
    }
}

fn check(ret: c_int) -> Result<()> {
    if ret == 0 { Ok(()) } else { Err(last_error()) }
}

// SAFETY: mmap returns page-aligned mappings that stay valid until munmap; every call only touches
// the range it is given.
unsafe impl VMem for Backend {
    #[inline]
    fn page_size() -> usize {
        match PAGE_SIZE.load(Ordering::Relaxed) {
            0 => {
                // SAFETY: no preconditions.
                let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
                PAGE_SIZE.store(size, Ordering::Relaxed);
                size
            }
            n => n,
        }
    }

    #[inline]
    fn allocation_granularity() -> usize {
        Self::page_size()
    }

    fn reserve(size: usize) -> Result<NonNull<u8>> {
        // SAFETY: an anonymous mapping at a kernel-chosen address has no preconditions.
        let p = unsafe { libc::mmap(ptr::null_mut(), size, libc::PROT_NONE, RESERVE_FLAGS, -1, 0) };
        if p == libc::MAP_FAILED {
            return Err(last_error());
        }
        // mmap never returns null on success.
        Ok(NonNull::new(p.cast()).unwrap())
    }

    fn reserve_aligned(size: usize, align: usize) -> Result<NonNull<u8>> {
        // Over-reserve, then unmap the misaligned head and the excess tail.
        let size = size.next_multiple_of(Self::page_size());
        let Some(total) = size.checked_add(align - Self::page_size()) else {
            return Err(crate::Error::from_kind(crate::ErrorKind::OutOfMemory));
        };
        let base = Self::reserve(total)?.as_ptr();
        let head = base.addr().next_multiple_of(align) - base.addr();
        let tail = total - head - size;
        // SAFETY: both ranges lie inside the mapping just made and are page aligned.
        unsafe {
            if head > 0 {
                libc::munmap(base.cast(), head);
            }
            if tail > 0 {
                libc::munmap(base.add(head + size).cast(), tail);
            }
            Ok(NonNull::new_unchecked(base.add(head)))
        }
    }

    unsafe fn release(base: NonNull<u8>, size: usize) -> Result<()> {
        // SAFETY: the caller guarantees this is exactly one live reservation.
        check(unsafe { libc::munmap(base.as_ptr().cast(), size) })
    }

    unsafe fn commit(ptr: NonNull<u8>, size: usize, prot: Protection) -> Result<()> {
        // SAFETY: the caller guarantees the range lies inside a live reservation.
        check(unsafe { libc::mprotect(ptr.as_ptr().cast(), size, prot_flags(prot)) })
    }

    unsafe fn decommit(ptr: NonNull<u8>, size: usize) -> Result<()> {
        // Mapping fresh PROT_NONE pages over the range drops the old pages on every POSIX system
        // and guarantees zeroes on the next commit, unlike madvise.
        // SAFETY: the caller guarantees the range lies inside a live reservation, so MAP_FIXED only
        // replaces pages we own.
        let p = unsafe {
            libc::mmap(
                ptr.as_ptr().cast(),
                size,
                libc::PROT_NONE,
                RESERVE_FLAGS | libc::MAP_FIXED,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            Err(last_error())
        } else {
            Ok(())
        }
    }

    unsafe fn protect(ptr: NonNull<u8>, size: usize, prot: Protection) -> Result<()> {
        // SAFETY: the caller guarantees the range is committed.
        check(unsafe { libc::mprotect(ptr.as_ptr().cast(), size, prot_flags(prot)) })
    }
}
