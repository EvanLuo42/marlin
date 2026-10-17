//! IME: the composition string from `WM_IME_COMPOSITION`, and placing the IME's windows next
//! to the text field. Committed text needs nothing here: `DefWindowProcW` turns it into
//! `WM_CHAR`s, which the hook already reads.

use core::ptr;

use alloc::string::String;
use alloc::vec::Vec;

use marlin_app::Rect;
use marlin_pal::os::windows::check;
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::UI::Input::Ime::{
    CANDIDATEFORM, CFS_EXCLUDE, CFS_FORCE_POSITION, COMPOSITIONFORM, GCS_COMPSTR, GCS_CURSORPOS,
    GCS_RESULTSTR, IACE_DEFAULT, ImmAssociateContextEx, ImmGetCompositionStringW, ImmGetContext,
    ImmReleaseContext, ImmSetCandidateWindow, ImmSetCompositionWindow,
};

use crate::Result;

/// The text field an IME is placed next to.
#[derive(Clone, Copy)]
pub(super) struct ImeArea {
    pub(super) hwnd: HWND,
    pub(super) area: Rect,
}

/// Enables the IME on `hwnd` and places it at `area`, or disables it.
pub(super) fn set_text_input(hwnd: HWND, area: Option<Rect>) -> Result<()> {
    match area {
        Some(area) => {
            // SAFETY: `hwnd` is a live window of this thread; IACE_DEFAULT restores its default
            // input context.
            check(unsafe { ImmAssociateContextEx(hwnd, ptr::null_mut(), IACE_DEFAULT) })?;
            place(&ImeArea { hwnd, area });
            Ok(())
        }
        // SAFETY: as above; a null context with no flags disables the IME for the window.
        None => check(unsafe { ImmAssociateContextEx(hwnd, ptr::null_mut(), 0) }),
    }
}

/// Puts the composition window at the field's start and the candidate list below the field,
/// never covering it.
pub(super) fn place(ime: &ImeArea) {
    let a = ime.area;
    let field = RECT {
        left: a.x,
        top: a.y,
        right: a.x + a.width as i32,
        bottom: a.y + a.height as i32,
    };
    // SAFETY: `hwnd` is a live window of this thread; the context is released below.
    let context = unsafe { ImmGetContext(ime.hwnd) };
    if context.is_null() {
        return;
    }
    let composition = COMPOSITIONFORM {
        dwStyle: CFS_FORCE_POSITION,
        ptCurrentPos: POINT { x: a.x, y: a.y },
        rcArea: field,
    };
    let candidate = CANDIDATEFORM {
        dwIndex: 0,
        dwStyle: CFS_EXCLUDE,
        ptCurrentPos: POINT {
            x: a.x,
            y: field.bottom,
        },
        rcArea: field,
    };
    // SAFETY: a live input context and valid forms.
    unsafe {
        ImmSetCompositionWindow(context, &composition);
        ImmSetCandidateWindow(context, &candidate);
        ImmReleaseContext(ime.hwnd, context);
    }
}

/// The composition after a `WM_IME_COMPOSITION`: `None` if unchanged, `Some(None)` if it ended,
/// else its text and the caret as a UTF-8 byte offset.
pub(super) fn composition(hwnd: HWND, lparam: LPARAM) -> Option<Option<(String, usize)>> {
    let flags = lparam as u32;
    if flags & GCS_COMPSTR == 0 {
        // Only a result: the composition is over (its text comes as WM_CHAR).
        return (flags & GCS_RESULTSTR != 0).then_some(None);
    }
    // SAFETY: `hwnd` is a live window of this thread; the context is released below.
    let context = unsafe { ImmGetContext(hwnd) };
    if context.is_null() {
        return None;
    }
    // SAFETY: a live context; a null buffer asks for the size in bytes.
    let bytes = unsafe { ImmGetCompositionStringW(context, GCS_COMPSTR, ptr::null_mut(), 0) };
    let mut wide: Vec<u16> = alloc::vec![0; bytes.max(0) as usize / 2];
    if !wide.is_empty() {
        // SAFETY: the buffer holds `bytes` bytes.
        unsafe {
            ImmGetCompositionStringW(context, GCS_COMPSTR, wide.as_mut_ptr().cast(), bytes as u32)
        };
    }
    // The caret, in UTF-16 units.
    let caret = if flags & GCS_CURSORPOS != 0 {
        // SAFETY: a live context; GCS_CURSORPOS returns the position, not a string.
        unsafe { ImmGetCompositionStringW(context, GCS_CURSORPOS, ptr::null_mut(), 0) }.max(0)
            as usize
    } else {
        wide.len()
    };
    // SAFETY: obtained above.
    unsafe { ImmReleaseContext(hwnd, context) };

    if wide.is_empty() {
        return Some(None);
    }
    let text: String = char::decode_utf16(wide.iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect();
    // The caret as a UTF-8 byte offset: count the bytes of the chars before it.
    let mut units = 0;
    let mut cursor = 0;
    for c in text.chars() {
        if units >= caret {
            break;
        }
        units += c.len_utf16();
        cursor += c.len_utf8();
    }
    Some(Some((text, cursor)))
}
