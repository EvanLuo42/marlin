//! Touch and pen from `WM_POINTER*` (Windows 8+). Mouse pointers are left to the mouse
//! messages, and touch and pen messages still reach `DefWindowProcW`, which promotes them to
//! mouse input for UI that only handles the mouse.

use core::mem;

use marlin_app::os::windows::Message;
use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::UI::Input::Pointer::{
    GetPointerInfo, GetPointerPenInfo, GetPointerType, POINTER_FLAG_CANCELED,
    POINTER_FLAG_INCONTACT, POINTER_INFO, POINTER_PEN_INFO,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    PEN_FLAG_BARREL, PEN_FLAG_ERASER, PEN_FLAG_INVERTED, POINTER_INPUT_TYPE, PT_PEN, PT_TOUCH,
    WM_POINTERCAPTURECHANGED, WM_POINTERDOWN, WM_POINTERLEAVE, WM_POINTERUP,
};

use crate::{InputEvent, PenSample, TouchPhase};

/// Translates a pointer message, or `None` for mouse pointers and failed queries.
pub(super) fn translate(m: &Message) -> Option<InputEvent> {
    let (window, hwnd, msg) = (m.window, m.hwnd, m.msg);
    // Capture moving within the window (the promoted mouse button makes the input hook capture
    // the mouse) does not end the touch; lparam is the window gaining capture.
    if msg == WM_POINTERCAPTURECHANGED && m.lparam as HWND == hwnd {
        return None;
    }
    let id = (m.wparam & 0xFFFF) as u32;
    let mut kind: POINTER_INPUT_TYPE = 0;
    // SAFETY: valid out pointer; fails for stale ids.
    if unsafe { GetPointerType(id, &mut kind) } == 0 {
        return None;
    }
    match kind {
        PT_TOUCH => {
            // SAFETY: all-zero is a valid POINTER_INFO.
            let mut info: POINTER_INFO = unsafe { mem::zeroed() };
            // SAFETY: valid out pointer.
            if unsafe { GetPointerInfo(id, &mut info) } == 0 {
                return None;
            }
            let phase = match msg {
                _ if info.pointerFlags & POINTER_FLAG_CANCELED != 0 => TouchPhase::Cancelled,
                WM_POINTERCAPTURECHANGED => TouchPhase::Cancelled,
                WM_POINTERDOWN => TouchPhase::Started,
                WM_POINTERUP => TouchPhase::Ended,
                // A touch leaving the window without lifting ends for us.
                WM_POINTERLEAVE if info.pointerFlags & POINTER_FLAG_INCONTACT != 0 => {
                    TouchPhase::Ended
                }
                WM_POINTERLEAVE => return None,
                _ => TouchPhase::Moved,
            };
            let [x, y] = client(hwnd, info.ptPixelLocation);
            Some(InputEvent::Touch {
                id,
                window,
                x,
                y,
                phase,
            })
        }
        PT_PEN => {
            if msg == WM_POINTERLEAVE || msg == WM_POINTERCAPTURECHANGED {
                return Some(InputEvent::PenLeft);
            }
            // SAFETY: all-zero is a valid POINTER_PEN_INFO.
            let mut pen: POINTER_PEN_INFO = unsafe { mem::zeroed() };
            // SAFETY: valid out pointer.
            if unsafe { GetPointerPenInfo(id, &mut pen) } == 0 {
                return None;
            }
            let info = &pen.pointerInfo;
            let contact = info.pointerFlags & POINTER_FLAG_INCONTACT != 0 && msg != WM_POINTERUP;
            Some(InputEvent::Pen(PenSample {
                window,
                position: client(hwnd, info.ptPixelLocation),
                // 0..=1024 while in contact.
                pressure: if contact {
                    pen.pressure.min(1024) as f32 / 1024.0
                } else {
                    0.0
                },
                tilt: [pen.tiltX as f32, pen.tiltY as f32],
                twist: pen.rotation as f32,
                contact,
                barrel: pen.penFlags & PEN_FLAG_BARREL != 0,
                eraser: pen.penFlags & (PEN_FLAG_ERASER | PEN_FLAG_INVERTED) != 0,
            }))
        }
        _ => None,
    }
}

fn client(hwnd: HWND, mut p: POINT) -> [f32; 2] {
    // SAFETY: `hwnd` is the live window receiving the message; valid in/out pointer.
    unsafe { ScreenToClient(hwnd, &mut p) };
    [p.x as f32, p.y as f32]
}
