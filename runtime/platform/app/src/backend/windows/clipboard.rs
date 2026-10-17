//! Clipboard text as `CF_UNICODETEXT`, opened by the helper window.

use core::ptr;

use alloc::string::String;
use alloc::vec::Vec;

use marlin_pal::os::windows::{check, last_error};
use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

use crate::Result;

/// Holds the clipboard open; closes it when dropped.
struct Open;

impl Open {
    fn new(owner: HWND) -> Result<Self> {
        // SAFETY: `owner` is the app's live helper window. Fails while another window has the
        // clipboard open.
        check(unsafe { OpenClipboard(owner) })?;
        Ok(Self)
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: this thread opened the clipboard.
        unsafe { CloseClipboard() };
    }
}

pub(super) fn text(owner: HWND) -> Result<Option<String>> {
    // SAFETY: no preconditions.
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT as u32) } == 0 {
        return Ok(None);
    }
    let _open = Open::new(owner)?;
    // SAFETY: the clipboard is open; the handle stays owned by the clipboard.
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT as u32) };
    if handle.is_null() {
        return Ok(None);
    }
    // SAFETY: CF_UNICODETEXT data is a global memory handle.
    let data = unsafe { GlobalLock(handle) }.cast::<u16>();
    if data.is_null() {
        return Err(last_error());
    }
    // SAFETY: the locked block holds `GlobalSize` bytes; the text is NUL-terminated within it,
    // but a malformed block is bounded by its size.
    let units = unsafe { core::slice::from_raw_parts(data, GlobalSize(handle) / 2) };
    let len = units.iter().position(|&c| c == 0).unwrap_or(units.len());
    let text = String::from_utf16_lossy(&units[..len]);
    // SAFETY: locked above.
    unsafe { GlobalUnlock(handle) };
    Ok(Some(text))
}

pub(super) fn set_text(owner: HWND, text: &str) -> Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let bytes = wide.len() * 2;
    // SAFETY: no preconditions.
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
    if memory.is_null() {
        return Err(last_error());
    }
    // SAFETY: `memory` is a fresh movable block of `bytes` bytes.
    let data = unsafe { GlobalLock(memory) }.cast::<u16>();
    if data.is_null() {
        let error = last_error();
        // SAFETY: allocated above and not handed out.
        unsafe { GlobalFree(memory) };
        return Err(error);
    }
    // SAFETY: the block holds `wide.len()` units; then unlock it for the clipboard.
    unsafe {
        ptr::copy_nonoverlapping(wide.as_ptr(), data, wide.len());
        GlobalUnlock(memory);
    }

    let result = Open::new(owner).and_then(|_open| {
        // SAFETY: the clipboard is open; on success it owns `memory`.
        check(unsafe { EmptyClipboard() })?;
        // SAFETY: as above.
        let set = unsafe { SetClipboardData(CF_UNICODETEXT as u32, memory as HANDLE) };
        if set.is_null() {
            return Err(last_error());
        }
        Ok(())
    });
    if result.is_err() {
        // SAFETY: the clipboard did not take ownership.
        unsafe { GlobalFree(memory) };
    }
    result
}
