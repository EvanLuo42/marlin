//! Large pages via `MEM_LARGE_PAGES`.
//!
//! The account must hold SeLockMemoryPrivilege ("Lock pages in memory"); the first query enables it
//! in the process token.

use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, GetLastError, HANDLE, LUID};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_LOCK_MEMORY_NAME,
    SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::System::Memory::{
    GetLargePageMinimum, MEM_COMMIT, MEM_LARGE_PAGES, MEM_RESERVE, VirtualAlloc,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::Backend;
use super::error::last_error;
use super::vmem::page_protection;
use crate::contract::ext::LargePages;
use crate::{Protection, Result};

/// 0 = not queried yet, [`UNAVAILABLE`], or the large page size.
static STATE: AtomicUsize = AtomicUsize::new(0);
const UNAVAILABLE: usize = 1;

// SAFETY: MEM_LARGE_PAGES allocations are aligned to the large page size and released with
// MEM_RELEASE like any other reservation.
unsafe impl LargePages for Backend {
    fn large_page_size() -> Option<usize> {
        let state = match STATE.load(Ordering::Relaxed) {
            0 => {
                let state = query();
                STATE.store(state, Ordering::Relaxed);
                state
            }
            state => state,
        };
        (state != UNAVAILABLE).then_some(state)
    }

    fn alloc_large(size: usize, prot: Protection) -> Result<NonNull<u8>> {
        let flags = MEM_RESERVE | MEM_COMMIT | MEM_LARGE_PAGES;
        // SAFETY: allocating fresh memory has no preconditions.
        let p = unsafe { VirtualAlloc(ptr::null(), size, flags, page_protection(prot)) };
        NonNull::new(p.cast()).ok_or_else(last_error)
    }
}

#[cold]
fn query() -> usize {
    // SAFETY: no preconditions.
    let size = unsafe { GetLargePageMinimum() };
    if size == 0 || !enable_lock_memory_privilege() {
        UNAVAILABLE
    } else {
        size
    }
}

/// Enables SeLockMemoryPrivilege in the process token. Only succeeds if the account holds it.
fn enable_lock_memory_privilege() -> bool {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo-handle; `token` is a valid out pointer.
    let opened = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    };
    if opened == 0 {
        return false;
    }

    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    // SAFETY: SE_LOCK_MEMORY_NAME is a static wide string; `luid` is a valid out pointer.
    let found = unsafe { LookupPrivilegeValueW(ptr::null(), SE_LOCK_MEMORY_NAME, &mut luid) } != 0;
    let enabled = found && {
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        // SAFETY: `token` was opened with TOKEN_ADJUST_PRIVILEGES; `privileges` outlives the call.
        // The call succeeds even when the privilege is not held, so GetLastError decides.
        unsafe {
            AdjustTokenPrivileges(token, 0, &privileges, 0, ptr::null_mut(), ptr::null_mut()) != 0
                && GetLastError() == ERROR_SUCCESS
        }
    };

    // SAFETY: `token` is a handle we own.
    unsafe { CloseHandle(token) };
    enabled
}
