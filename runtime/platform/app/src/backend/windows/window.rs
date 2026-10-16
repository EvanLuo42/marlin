use core::cell::Cell;
use core::ptr::{self, NonNull};

use alloc::boxed::Box;
use alloc::rc::Rc;

use marlin_pal::os::windows::{check, last_error};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint, MonitorFromWindow,
};
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DestroyWindow, GWL_STYLE, GetClientRect, GetWindowLongPtrW,
    GetWindowPlacement, HWND_TOP, MNC_CLOSE, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, SC_KEYMENU,
    SIZE_MINIMIZED, SW_HIDE, SW_SHOW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPlacement,
    SetWindowPos, SetWindowTextW, ShowWindow, WINDOW_STYLE, WINDOWPLACEMENT, WM_CLOSE,
    WM_DPICHANGED, WM_ERASEBKGND, WM_KILLFOCUS, WM_MENUCHAR, WM_POWERBROADCAST, WM_SETFOCUS,
    WM_SIZE, WM_SYSCOMMAND, WS_EX_APPWINDOW, WS_MAXIMIZEBOX, WS_OVERLAPPEDWINDOW, WS_POPUP,
    WS_THICKFRAME, WS_VISIBLE,
};

use super::{CLASS_NAME, Shared};
use crate::contract::Window as _;
use crate::os::windows::Message;
use crate::{Event, Result, Size, Surface, WindowDesc, WindowId, WindowMode};

/// Per-window state, boxed so the window procedure can hold a stable pointer to it.
pub(super) struct WindowState {
    id: WindowId,
    shared: Rc<Shared>,
    /// Null while `CreateWindowExW` runs; events are not reported until it returns.
    hwnd: Cell<HWND>,
    windowed_style: WINDOW_STYLE,
    size: Cell<Size>,
    dpi: Cell<u32>,
    mode: Cell<WindowMode>,
    minimized: Cell<bool>,
    focused: Cell<bool>,
    /// Windowed position and size, saved on entering fullscreen.
    placement: Cell<WINDOWPLACEMENT>,
}

impl WindowState {
    /// Handles a message for this window. `None` falls through to `DefWindowProcW`.
    pub(super) fn handle(
        &self,
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        let created = !self.hwnd.get().is_null();
        let own = match msg {
            WM_CLOSE => {
                self.shared.push(Event::CloseRequested(self.id));
                Some(0)
            }
            WM_SIZE => {
                self.on_size(wparam as u32, lparam, created);
                Some(0)
            }
            WM_DPICHANGED => {
                let dpi = (wparam & 0xFFFF) as u32;
                self.dpi.set(dpi);
                if created {
                    self.shared.push(Event::DpiChanged {
                        window: self.id,
                        dpi,
                    });
                }
                if self.mode.get() == WindowMode::Windowed {
                    // SAFETY: for WM_DPICHANGED, lparam points to the suggested window RECT.
                    let r = unsafe { *(lparam as *const RECT) };
                    // SAFETY: `hwnd` is this live window. Re-enters with WM_SIZE (no borrows held).
                    unsafe {
                        SetWindowPos(
                            hwnd,
                            ptr::null_mut(),
                            r.left,
                            r.top,
                            r.right - r.left,
                            r.bottom - r.top,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        )
                    };
                }
                Some(0)
            }
            WM_SETFOCUS | WM_KILLFOCUS => {
                let focused = msg == WM_SETFOCUS;
                if self.focused.replace(focused) != focused && created {
                    self.shared.push(Event::Focused {
                        window: self.id,
                        focused,
                    });
                }
                None
            }
            WM_POWERBROADCAST => {
                match wparam as u32 {
                    PBT_APMSUSPEND => self.shared.set_suspended(true),
                    PBT_APMRESUMEAUTOMATIC => self.shared.set_suspended(false),
                    _ => {}
                }
                Some(1)
            }
            // A bare Alt or F10 would enter menu mode, which runs a modal loop and stalls the
            // game. Alt+Space (lparam = ' ') still opens the system menu.
            WM_SYSCOMMAND if (wparam & 0xFFF0) as u32 == SC_KEYMENU && lparam == 0 => Some(0),
            // Alt+<key> with no menu would beep.
            WM_MENUCHAR => Some((MNC_CLOSE as LRESULT) << 16),
            // The swapchain covers the client area; erasing first only flickers.
            WM_ERASEBKGND => Some(1),
            _ => None,
        };

        let hooked = self.shared.run_hooks(&Message {
            window: self.id,
            hwnd,
            msg,
            wparam,
            lparam,
        });
        own.or(hooked)
    }

    fn on_size(&self, kind: u32, lparam: LPARAM, created: bool) {
        let size = Size::new((lparam & 0xFFFF) as u32, ((lparam >> 16) & 0xFFFF) as u32);
        if kind == SIZE_MINIMIZED {
            if !self.minimized.replace(true) && created {
                self.shared.push(Event::Minimized(self.id));
            }
            return;
        }
        if self.minimized.replace(false) && created {
            self.shared.push(Event::Restored(self.id));
        }
        if size.width > 0 && size.height > 0 && self.size.replace(size) != size && created {
            self.shared.push(Event::Resized {
                window: self.id,
                size,
            });
        }
    }

    fn hwnd(&self) -> HWND {
        self.hwnd.get()
    }
}

pub(crate) struct Window {
    state: Box<WindowState>,
}

impl Window {
    pub(super) fn new(shared: &Rc<Shared>, id: WindowId, desc: &WindowDesc<'_>) -> Result<Self> {
        let mut style = WS_OVERLAPPEDWINDOW;
        if !desc.resizable {
            style &= !(WS_THICKFRAME | WS_MAXIMIZEBOX);
        }
        let ex_style = WS_EX_APPWINDOW;

        // Size for the primary monitor's DPI; CW_USEDEFAULT places new windows there. A different
        // monitor sends WM_DPICHANGED, which rescales.
        let dpi = primary_monitor_dpi();
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: scale(desc.width, dpi),
            bottom: scale(desc.height, dpi),
        };
        // SAFETY: `rect` is a valid in/out pointer.
        unsafe { AdjustWindowRectExForDpi(&mut rect, style, 0, ex_style, dpi) };

        let state = Box::new(WindowState {
            id,
            shared: shared.clone(),
            hwnd: Cell::new(ptr::null_mut()),
            windowed_style: style,
            size: Cell::new(Size::default()),
            dpi: Cell::new(dpi),
            mode: Cell::new(WindowMode::Windowed),
            minimized: Cell::new(false),
            focused: Cell::new(false),
            placement: Cell::new(empty_placement()),
        });

        let mut title = [0u16; 256];
        // SAFETY: the class is registered; `title` is NUL-terminated; the state pointer stays
        // valid for the window's life (see `window_proc`).
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                CLASS_NAME,
                encode_wide(desc.title, &mut title),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                rect.right - rect.left,
                rect.bottom - rect.top,
                ptr::null_mut(),
                ptr::null_mut(),
                shared.hinstance,
                (&raw const *state).cast(),
            )
        };
        if hwnd.is_null() {
            return Err(last_error());
        }
        state.hwnd.set(hwnd);
        // SAFETY: `hwnd` is live.
        state.dpi.set(unsafe { GetDpiForWindow(hwnd) }.max(1));
        let mut client = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: `hwnd` is live; `client` is a valid out pointer.
        unsafe { GetClientRect(hwnd, &mut client) };
        state.size.set(Size::new(
            client.right.max(1) as u32,
            client.bottom.max(1) as u32,
        ));

        let window = Self { state };
        if desc.mode != WindowMode::Windowed {
            window.set_mode(desc.mode)?;
        }
        if desc.visible {
            window.set_visible(true);
        }
        Ok(window)
    }

    pub(crate) fn hwnd(&self) -> HWND {
        self.state.hwnd()
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the window is live and owned by this thread. Its final messages still see
        // `state`, which is freed only after this returns.
        unsafe { DestroyWindow(self.hwnd()) };
    }
}

impl crate::contract::Window for Window {
    fn id(&self) -> WindowId {
        self.state.id
    }

    fn surface(&self) -> Surface {
        Surface::Win32 {
            hwnd: NonNull::new(self.hwnd()).expect("live window"),
            hinstance: NonNull::new(self.state.shared.hinstance).expect("module handle"),
        }
    }

    fn size(&self) -> Size {
        self.state.size.get()
    }

    fn dpi(&self) -> u32 {
        self.state.dpi.get()
    }

    fn mode(&self) -> WindowMode {
        self.state.mode.get()
    }

    fn set_mode(&self, mode: WindowMode) -> Result<()> {
        let state = &*self.state;
        if state.mode.get() == mode {
            return Ok(());
        }
        let hwnd = self.hwnd();
        // SAFETY: `hwnd` is live.
        let visible = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32 & WS_VISIBLE;
        match mode {
            WindowMode::BorderlessFullscreen => {
                let mut placement = empty_placement();
                // SAFETY: `hwnd` is live; `placement.length` is set.
                check(unsafe { GetWindowPlacement(hwnd, &mut placement) })?;
                state.placement.set(placement);

                let mut info = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    rcMonitor: RECT {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    },
                    rcWork: RECT {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    },
                    dwFlags: 0,
                };
                // SAFETY: `hwnd` is live; `info.cbSize` is set.
                check(unsafe {
                    GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info)
                })?;
                let r = info.rcMonitor;
                // Set first: the resize below must not apply WM_DPICHANGED's windowed rect.
                state.mode.set(mode);
                // SAFETY: `hwnd` is live. Both calls re-enter the window procedure with no borrows
                // held.
                unsafe {
                    SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_POPUP | visible) as isize);
                    SetWindowPos(
                        hwnd,
                        HWND_TOP,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_FRAMECHANGED | SWP_NOOWNERZORDER,
                    );
                }
            }
            WindowMode::Windowed => {
                state.mode.set(mode);
                let placement = state.placement.get();
                // SAFETY: `hwnd` is live; `placement` came from GetWindowPlacement.
                unsafe {
                    SetWindowLongPtrW(hwnd, GWL_STYLE, (state.windowed_style | visible) as isize);
                    SetWindowPlacement(hwnd, &placement);
                    SetWindowPos(
                        hwnd,
                        ptr::null_mut(),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE
                            | SWP_NOSIZE
                            | SWP_NOZORDER
                            | SWP_NOOWNERZORDER
                            | SWP_FRAMECHANGED,
                    );
                }
            }
        }
        Ok(())
    }

    fn set_title(&self, title: &str) {
        let mut buf = [0u16; 256];
        // SAFETY: `hwnd` is live; the buffer is NUL-terminated.
        unsafe { SetWindowTextW(self.hwnd(), encode_wide(title, &mut buf)) };
    }

    fn set_visible(&self, visible: bool) {
        // SAFETY: `hwnd` is live.
        unsafe { ShowWindow(self.hwnd(), if visible { SW_SHOW } else { SW_HIDE }) };
    }

    fn is_minimized(&self) -> bool {
        self.state.minimized.get()
    }

    fn is_focused(&self) -> bool {
        self.state.focused.get()
    }
}

fn primary_monitor_dpi() -> u32 {
    let (mut x, mut y) = (0, 0);
    // SAFETY: MONITOR_DEFAULTTOPRIMARY always yields a monitor; the out pointers are valid.
    let hr = unsafe {
        GetDpiForMonitor(
            MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY),
            MDT_EFFECTIVE_DPI,
            &mut x,
            &mut y,
        )
    };
    if hr >= 0 && x > 0 { x } else { 96 }
}

fn scale(logical: u32, dpi: u32) -> i32 {
    (logical as u64 * dpi as u64 / 96).min(i32::MAX as u64) as i32
}

fn empty_placement() -> WINDOWPLACEMENT {
    let zero = POINT { x: 0, y: 0 };
    WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        flags: 0,
        showCmd: 0,
        ptMinPosition: zero,
        ptMaxPosition: zero,
        rcNormalPosition: RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
    }
}

/// Encodes `s` as NUL-terminated UTF-16 into `buf`, truncating at a character boundary.
fn encode_wide<const N: usize>(s: &str, buf: &mut [u16; N]) -> *const u16 {
    let mut len = 0;
    for c in s.chars() {
        if len + c.len_utf16() >= N {
            break;
        }
        len += c.encode_utf16(&mut buf[len..]).len();
    }
    buf[len] = 0;
    buf.as_ptr()
}
