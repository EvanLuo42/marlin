use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicU32, Ordering};

use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_DECOMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
    PAGE_NOACCESS, PAGE_PROTECTION_FLAGS, PAGE_READONLY, PAGE_READWRITE, VirtualAlloc, VirtualFree,
    VirtualProtect,
};
use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};

use super::Backend;
use super::error::{check, last_error};
use crate::contract::VMem;
use crate::{Protection, Result};

// Cached `SYSTEM_INFO` fields; 0 means not queried yet. Racing first queries store equal values.
static PAGE_SIZE: AtomicU32 = AtomicU32::new(0);
static GRANULARITY: AtomicU32 = AtomicU32::new(0);

#[cold]
fn query_system_info() -> (u32, u32) {
    let mut info = SYSTEM_INFO::default();
    // SAFETY: `info` is a valid out pointer.
    unsafe { GetSystemInfo(&mut info) };
    PAGE_SIZE.store(info.dwPageSize, Ordering::Relaxed);
    GRANULARITY.store(info.dwAllocationGranularity, Ordering::Relaxed);
    (info.dwPageSize, info.dwAllocationGranularity)
}

pub(super) fn page_protection(prot: Protection) -> PAGE_PROTECTION_FLAGS {
    match prot {
        Protection::NoAccess => PAGE_NOACCESS,
        Protection::Read => PAGE_READONLY,
        Protection::ReadWrite => PAGE_READWRITE,
        Protection::ReadExecute => PAGE_EXECUTE_READ,
        Protection::ReadWriteExecute => PAGE_EXECUTE_READWRITE,
    }
}

// SAFETY: VirtualAlloc reservations are aligned to the allocation granularity and stay valid until
// MEM_RELEASE; every call only touches the range it is given.
unsafe impl VMem for Backend {
    #[inline]
    fn page_size() -> usize {
        match PAGE_SIZE.load(Ordering::Relaxed) {
            0 => query_system_info().0 as usize,
            n => n as usize,
        }
    }

    #[inline]
    fn allocation_granularity() -> usize {
        match GRANULARITY.load(Ordering::Relaxed) {
            0 => query_system_info().1 as usize,
            n => n as usize,
        }
    }

    fn reserve(size: usize) -> Result<NonNull<u8>> {
        // SAFETY: reserving fresh address space has no preconditions.
        let p = unsafe { VirtualAlloc(ptr::null(), size, MEM_RESERVE, PAGE_NOACCESS) };
        NonNull::new(p.cast()).ok_or_else(last_error)
    }

    unsafe fn release(base: NonNull<u8>, _size: usize) -> Result<()> {
        // SAFETY: the caller guarantees `base` starts a live reservation. MEM_RELEASE frees the
        // whole reservation and requires a size of 0.
        check(unsafe { VirtualFree(base.as_ptr().cast(), 0, MEM_RELEASE) })
    }

    unsafe fn commit(ptr: NonNull<u8>, size: usize, prot: Protection) -> Result<()> {
        // SAFETY: the caller guarantees the range lies inside a live reservation.
        let p =
            unsafe { VirtualAlloc(ptr.as_ptr().cast(), size, MEM_COMMIT, page_protection(prot)) };
        if p.is_null() {
            Err(last_error())
        } else {
            Ok(())
        }
    }

    unsafe fn decommit(ptr: NonNull<u8>, size: usize) -> Result<()> {
        // SAFETY: the caller guarantees the range lies inside a live reservation; `size` is non-zero,
        // so this cannot decommit the whole region by accident.
        check(unsafe { VirtualFree(ptr.as_ptr().cast(), size, MEM_DECOMMIT) })
    }

    unsafe fn protect(ptr: NonNull<u8>, size: usize, prot: Protection) -> Result<()> {
        let mut old = 0;
        // SAFETY: the caller guarantees the range is committed; `old` is a valid out pointer.
        check(unsafe { VirtualProtect(ptr.as_ptr().cast(), size, page_protection(prot), &mut old) })
    }
}
