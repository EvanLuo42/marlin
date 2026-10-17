//! Mirrored mappings with placeholders (`VirtualAlloc2` / `MapViewOfFile3`, Windows 10 1803+), and
//! `MEM_RESET`.

use core::ffi::c_void;
use core::ptr::{self, NonNull};

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MEM_PRESERVE_PLACEHOLDER, MEM_RELEASE, MEM_REPLACE_PLACEHOLDER,
    MEM_RESERVE, MEM_RESERVE_PLACEHOLDER, MEM_RESET, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile3,
    PAGE_NOACCESS, PAGE_READWRITE, UnmapViewOfFile, VirtualAlloc, VirtualAlloc2, VirtualFree,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::Backend;
use super::error::{check, last_error};
use crate::Result;
use crate::contract::ext::VmemAlias;

// SAFETY: both views map the same section, at `base` and `base + size`, until `mirror_free`.
unsafe impl VmemAlias for Backend {
    fn mirror_alloc(size: usize) -> Result<NonNull<u8>> {
        // SAFETY: each call below either creates something we own or works on the placeholder
        // reserved here; failures release what exists so far.
        unsafe {
            let process = GetCurrentProcess();
            let base = VirtualAlloc2(
                process,
                ptr::null(),
                2 * size,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                ptr::null_mut(),
                0,
            );
            if base.is_null() {
                return Err(last_error());
            }
            // Split the placeholder in two.
            if VirtualFree(base, size, MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER) == 0 {
                let error = last_error();
                VirtualFree(base, 0, MEM_RELEASE);
                return Err(error);
            }
            let second = base.cast::<u8>().add(size).cast::<c_void>();
            let section = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                ptr::null(),
                PAGE_READWRITE,
                (size as u64 >> 32) as u32,
                size as u32,
                ptr::null(),
            );
            if section.is_null() {
                let error = last_error();
                VirtualFree(base, 0, MEM_RELEASE);
                VirtualFree(second, 0, MEM_RELEASE);
                return Err(error);
            }
            let map = |at: *mut c_void| {
                MapViewOfFile3(
                    section,
                    process,
                    at,
                    0,
                    size,
                    MEM_REPLACE_PLACEHOLDER,
                    PAGE_READWRITE,
                    ptr::null_mut(),
                    0,
                )
                .Value
            };
            let first_view = map(base);
            let second_view = if first_view.is_null() {
                ptr::null_mut()
            } else {
                map(second)
            };
            let error = last_error();
            // The views keep the section alive.
            CloseHandle(section);
            if first_view.is_null() || second_view.is_null() {
                if first_view.is_null() {
                    VirtualFree(base, 0, MEM_RELEASE);
                } else {
                    UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: first_view });
                }
                VirtualFree(second, 0, MEM_RELEASE);
                return Err(error);
            }
            Ok(NonNull::new_unchecked(base.cast()))
        }
    }

    unsafe fn mirror_free(base: NonNull<u8>, size: usize) -> Result<()> {
        // SAFETY: the caller passes exactly the two views `mirror_alloc` made.
        unsafe {
            let first = MEMORY_MAPPED_VIEW_ADDRESS {
                Value: base.as_ptr().cast(),
            };
            let second = MEMORY_MAPPED_VIEW_ADDRESS {
                Value: base.as_ptr().add(size).cast(),
            };
            check(UnmapViewOfFile(first))?;
            check(UnmapViewOfFile(second))
        }
    }

    unsafe fn reset(ptr: NonNull<u8>, size: usize) -> Result<()> {
        // SAFETY: the caller guarantees the range is committed; MEM_RESET ignores the protection
        // but requires a valid one.
        let p = unsafe { VirtualAlloc(ptr.as_ptr().cast(), size, MEM_RESET, PAGE_NOACCESS) };
        if p.is_null() {
            Err(last_error())
        } else {
            Ok(())
        }
    }
}
