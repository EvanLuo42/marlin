use core::cell::{Cell, RefCell};
use core::ptr::{self, NonNull};

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use marlin_pal::os::windows::{check, last_error};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint,
    MonitorFromWindow, ScreenToClient,
};
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI,
};
use windows_sys::Win32::UI::Shell::{
    DragAcceptFiles, DragFinish, DragQueryFileW, DragQueryPoint, HDROP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, ChangeWindowMessageFilterEx, ClipCursor, CreateWindowExW, DestroyWindow,
    GWL_EXSTYLE, GWL_STYLE, GetClientRect, GetCursorPos, GetWindowLongPtrW, GetWindowPlacement,
    GetWindowRect, HTCLIENT, HWND_TOP, IDC_APPSTARTING, IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_IBEAM,
    IDC_NO, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IDC_WAIT,
    IsWindowVisible, LoadCursorW, MINMAXINFO, MNC_CLOSE, MSGFLT_ALLOW, SC_KEYMENU, SIZE_MINIMIZED,
    SW_HIDE, SW_SHOW, SWP_FRAMECHANGED, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SetCursor, SetCursorPos,
    SetWindowLongPtrW, SetWindowPlacement, SetWindowPos, SetWindowTextW, ShowWindow,
    WINDOW_EX_STYLE, WINDOW_STYLE, WINDOWPLACEMENT, WINDOWPOS, WM_CLOSE, WM_COPYDATA,
    WM_DPICHANGED, WM_DROPFILES, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KILLFOCUS, WM_MENUCHAR,
    WM_MOVE, WM_SETCURSOR, WM_SETFOCUS, WM_SIZE, WM_SYSCOMMAND, WM_WINDOWPOSCHANGED,
    WS_EX_APPWINDOW, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_OVERLAPPEDWINDOW, WS_POPUP, WS_SYSMENU,
    WS_THICKFRAME, WS_VISIBLE, WindowFromPoint,
};

use super::{CLASS_NAME, Shared, display};
use crate::contract::Window as _;
use crate::os::windows::Message;
use crate::{
    CursorGrab, CursorIcon, Display, DisplayId, Error, ErrorKind, Event, Point, Result, Size,
    Surface, WindowDesc, WindowId, WindowMode,
};

/// `WM_COPYGLOBALDATA`, which drag and drop from a lower-integrity process also needs.
const WM_COPYGLOBALDATA: u32 = 0x0049;

/// Per-window state, boxed so the window procedure can hold a stable pointer to it.
pub(super) struct WindowState {
    id: WindowId,
    shared: Rc<Shared>,
    /// Null while `CreateWindowExW` runs; events are not reported until it returns.
    hwnd: Cell<HWND>,
    windowed_style: WINDOW_STYLE,
    size: Cell<Size>,
    position: Cell<Point>,
    min_size: Cell<Option<Size>>,
    max_size: Cell<Option<Size>>,
    dpi: Cell<u32>,
    mode: Cell<WindowMode>,
    visible: Cell<bool>,
    minimized: Cell<bool>,
    focused: Cell<bool>,
    /// Windowed position and size, saved on entering fullscreen.
    placement: Cell<WINDOWPLACEMENT>,
    cursor_visible: Cell<bool>,
    cursor_icon: Cell<CursorIcon>,
    grab: Cell<CursorGrab>,
    /// This window set the current `ClipCursor` rectangle, so it must release it.
    clipping: Cell<bool>,
    dropped: RefCell<Vec<String>>,
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
                self.update_clip();
                Some(0)
            }
            WM_MOVE => {
                let position = Point::new(
                    (lparam & 0xFFFF) as i16 as i32,
                    ((lparam >> 16) & 0xFFFF) as i16 as i32,
                );
                // Minimized windows move to (-32000, -32000); that is not a real position.
                if !self.minimized.get() && self.position.replace(position) != position && created {
                    self.shared.push(Event::Moved {
                        window: self.id,
                        position,
                    });
                }
                self.update_clip();
                Some(0)
            }
            WM_WINDOWPOSCHANGED => {
                // SAFETY: for WM_WINDOWPOSCHANGED, lparam points to a WINDOWPOS.
                let flags = unsafe { (*(lparam as *const WINDOWPOS)).flags };
                let visible = match flags {
                    f if f & SWP_SHOWWINDOW != 0 => Some(true),
                    f if f & SWP_HIDEWINDOW != 0 => Some(false),
                    _ => None,
                };
                if let Some(visible) = visible
                    && self.visible.replace(visible) != visible
                    && created
                {
                    self.shared.push(Event::VisibilityChanged {
                        window: self.id,
                        visible,
                    });
                }
                // DefWindowProcW turns this into WM_SIZE and WM_MOVE.
                None
            }
            WM_GETMINMAXINFO => {
                self.on_get_min_max_info(hwnd, lparam);
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
                self.update_clip();
                None
            }
            WM_SETCURSOR if (lparam & 0xFFFF) as u32 == HTCLIENT => {
                self.apply_cursor();
                Some(1)
            }
            WM_DROPFILES => {
                self.on_drop(wparam as HDROP);
                Some(0)
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

    /// Turns the client-area size limits into the window-size limits Windows enforces.
    fn on_get_min_max_info(&self, hwnd: HWND, lparam: LPARAM) {
        if self.mode.get() != WindowMode::Windowed {
            return;
        }
        // SAFETY: for WM_GETMINMAXINFO, lparam points to a MINMAXINFO prefilled with defaults.
        let info = unsafe { &mut *(lparam as *mut MINMAXINFO) };
        let frame = |size: Size| {
            let r = self.frame_rect(hwnd, Point::default(), size);
            POINT {
                x: r.right - r.left,
                y: r.bottom - r.top,
            }
        };
        if let Some(min) = self.min_size.get() {
            info.ptMinTrackSize = frame(min);
        }
        if let Some(max) = self.max_size.get() {
            info.ptMaxTrackSize = frame(max);
        }
    }

    fn on_drop(&self, drop: HDROP) {
        // SAFETY: `drop` comes from this WM_DROPFILES and is freed below, after its last use.
        unsafe {
            let count = DragQueryFileW(drop, u32::MAX, ptr::null_mut(), 0);
            let mut point = POINT { x: 0, y: 0 };
            DragQueryPoint(drop, &mut point);
            let mut wide = Vec::new();
            for i in 0..count {
                let len = DragQueryFileW(drop, i, ptr::null_mut(), 0);
                wide.clear();
                wide.resize(len as usize + 1, 0u16);
                let len = DragQueryFileW(drop, i, wide.as_mut_ptr(), wide.len() as u32);
                self.dropped
                    .borrow_mut()
                    .push(String::from_utf16_lossy(&wide[..len as usize]));
            }
            DragFinish(drop);
            self.shared.push(Event::FilesDropped {
                window: self.id,
                count,
                position: Point::new(point.x, point.y),
            });
        }
    }

    /// The window rectangle whose client area is `size` at `client_origin`, for the current
    /// style and DPI.
    fn frame_rect(&self, hwnd: HWND, client_origin: Point, size: Size) -> RECT {
        let mut r = RECT {
            left: client_origin.x,
            top: client_origin.y,
            right: client_origin.x + size.width.min(i32::MAX as u32) as i32,
            bottom: client_origin.y + size.height.min(i32::MAX as u32) as i32,
        };
        // SAFETY: `hwnd` is live (or being created, which has its styles); `r` is a valid
        // in/out pointer.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as WINDOW_STYLE;
            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as WINDOW_EX_STYLE;
            AdjustWindowRectExForDpi(&mut r, style, 0, ex_style, self.dpi.get());
        }
        r
    }

    /// Sets the cursor shape for the client area: the chosen icon, or none while hidden or
    /// locked.
    fn apply_cursor(&self) {
        let hidden = !self.cursor_visible.get() || self.grab.get() == CursorGrab::Lock;
        let cursor = if hidden {
            ptr::null_mut()
        } else {
            let id = match self.cursor_icon.get() {
                CursorIcon::Arrow => IDC_ARROW,
                CursorIcon::Text => IDC_IBEAM,
                CursorIcon::Hand => IDC_HAND,
                CursorIcon::Crosshair => IDC_CROSS,
                CursorIcon::Progress => IDC_APPSTARTING,
                CursorIcon::Wait => IDC_WAIT,
                CursorIcon::NotAllowed => IDC_NO,
                CursorIcon::Move => IDC_SIZEALL,
                CursorIcon::ResizeHorizontal => IDC_SIZEWE,
                CursorIcon::ResizeVertical => IDC_SIZENS,
                CursorIcon::ResizeDiagonalDown => IDC_SIZENWSE,
                CursorIcon::ResizeDiagonalUp => IDC_SIZENESW,
            };
            // SAFETY: loading a predefined system cursor has no preconditions; shared cursors
            // need no cleanup.
            unsafe { LoadCursorW(ptr::null_mut(), id) }
        };
        // SAFETY: a shared system cursor or null.
        unsafe { SetCursor(cursor) };
    }

    /// Applies the cursor shape now if the cursor is over the client area, instead of waiting for
    /// the next mouse move.
    fn refresh_cursor(&self) {
        let hwnd = self.hwnd.get();
        let mut p = POINT { x: 0, y: 0 };
        let mut client = empty_rect();
        // SAFETY: `hwnd` is live; valid out pointers.
        let inside = unsafe {
            GetCursorPos(&mut p) != 0
                && WindowFromPoint(p) == hwnd
                && ScreenToClient(hwnd, &mut p) != 0
                && GetClientRect(hwnd, &mut client) != 0
        } && p.x >= 0
            && p.y >= 0
            && p.x < client.right
            && p.y < client.bottom;
        if inside {
            self.apply_cursor();
        }
    }

    /// Applies or releases the cursor clip for the current grab, focus and size.
    fn update_clip(&self) {
        let hwnd = self.hwnd.get();
        if hwnd.is_null() {
            return;
        }
        let active = self.focused.get() && !self.minimized.get();
        let grab = if active {
            self.grab.get()
        } else {
            CursorGrab::None
        };
        if grab == CursorGrab::None {
            if self.clipping.replace(false) {
                // SAFETY: releasing the clip this window set.
                unsafe { ClipCursor(ptr::null()) };
            }
            return;
        }
        let mut client = empty_rect();
        let mut origin = POINT { x: 0, y: 0 };
        // SAFETY: `hwnd` is live; valid out pointers.
        unsafe {
            GetClientRect(hwnd, &mut client);
            ClientToScreen(hwnd, &mut origin);
        }
        let mut r = RECT {
            left: origin.x,
            top: origin.y,
            right: origin.x + client.right,
            bottom: origin.y + client.bottom,
        };
        if grab == CursorGrab::Lock {
            // Pin the cursor to the center so clicks always land in the window.
            let (x, y) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
            r = RECT {
                left: x,
                top: y,
                right: x + 1,
                bottom: y + 1,
            };
        }
        self.clipping.set(true);
        // SAFETY: a valid rectangle.
        unsafe { ClipCursor(&r) };
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
        let mut style = if desc.decorated {
            WS_OVERLAPPEDWINDOW
        } else {
            WS_POPUP | WS_SYSMENU | WS_MINIMIZEBOX | WS_THICKFRAME
        };
        if !desc.resizable {
            style &= !(WS_THICKFRAME | WS_MAXIMIZEBOX);
        }
        let owner = desc.owner.map_or(ptr::null_mut(), |w| w.inner.hwnd());
        // Owned windows get no taskbar button.
        let ex_style = if owner.is_null() { WS_EX_APPWINDOW } else { 0 };

        // Size for the target display's DPI; CW_USEDEFAULT places new windows on the primary
        // one. A different display sends WM_DPICHANGED, which rescales.
        let monitor = match desc.display {
            Some(id) => {
                let monitor = id.0 as HMONITOR;
                display::monitor_info(monitor).ok_or(Error::from_kind(ErrorKind::NotFound))?;
                monitor
            }
            // SAFETY: MONITOR_DEFAULTTOPRIMARY always yields a monitor.
            None => unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) },
        };
        let dpi = monitor_dpi(monitor);
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: scale(desc.width, dpi),
            bottom: scale(desc.height, dpi),
        };
        // SAFETY: `rect` is a valid in/out pointer.
        unsafe { AdjustWindowRectExForDpi(&mut rect, style, 0, ex_style, dpi) };
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        let (x, y) = match desc.display {
            Some(_) => {
                let work = display::monitor_info(monitor)
                    .map_or_else(empty_rect, |info| info.monitorInfo.rcWork);
                centered(&work, width, height)
            }
            None => (CW_USEDEFAULT, CW_USEDEFAULT),
        };

        let state = Box::new(WindowState {
            id,
            shared: shared.clone(),
            hwnd: Cell::new(ptr::null_mut()),
            windowed_style: style,
            size: Cell::new(Size::default()),
            position: Cell::new(Point::default()),
            min_size: Cell::new(None),
            max_size: Cell::new(None),
            dpi: Cell::new(dpi),
            mode: Cell::new(WindowMode::Windowed),
            visible: Cell::new(false),
            minimized: Cell::new(false),
            focused: Cell::new(false),
            placement: Cell::new(empty_placement()),
            cursor_visible: Cell::new(true),
            cursor_icon: Cell::new(CursorIcon::Arrow),
            grab: Cell::new(CursorGrab::None),
            clipping: Cell::new(false),
            dropped: RefCell::new(Vec::new()),
        });

        let mut title = [0u16; 256];
        // SAFETY: the class is registered; `title` is NUL-terminated; `owner` is null or a live
        // window of this thread; the state pointer stays valid for the window's life (see
        // `window_proc`).
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                CLASS_NAME,
                encode_wide(desc.title, &mut title),
                style,
                x,
                y,
                width,
                height,
                owner,
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
        let mut client = empty_rect();
        let mut origin = POINT { x: 0, y: 0 };
        // SAFETY: `hwnd` is live; valid out pointers.
        unsafe {
            GetClientRect(hwnd, &mut client);
            ClientToScreen(hwnd, &mut origin);
        }
        state.size.set(Size::new(
            client.right.max(1) as u32,
            client.bottom.max(1) as u32,
        ));
        state.position.set(Point::new(origin.x, origin.y));

        if desc.accept_files {
            // SAFETY: `hwnd` is live. The filters let an elevated game accept drops from a normal
            // Explorer; failures only lose that case.
            unsafe {
                DragAcceptFiles(hwnd, 1);
                for msg in [WM_DROPFILES, WM_COPYDATA, WM_COPYGLOBALDATA] {
                    ChangeWindowMessageFilterEx(hwnd, msg, MSGFLT_ALLOW, ptr::null_mut());
                }
            }
        }

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

    /// Moves and sizes the window to cover `monitor`.
    fn cover(&self, monitor: HMONITOR) -> Result<()> {
        let info = display::monitor_info(monitor).ok_or(Error::from_kind(ErrorKind::NotFound))?;
        let r = info.monitorInfo.rcMonitor;
        // SAFETY: `hwnd` is live. Re-enters the window procedure with no borrows held.
        check(unsafe {
            SetWindowPos(
                self.hwnd(),
                HWND_TOP,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_FRAMECHANGED | SWP_NOOWNERZORDER,
            )
        })
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        if self.state.clipping.get() {
            // SAFETY: releasing the clip this window set.
            unsafe { ClipCursor(ptr::null()) };
        }
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

    fn set_size(&self, size: Size) {
        if self.state.mode.get() != WindowMode::Windowed {
            return;
        }
        let r = self.state.frame_rect(self.hwnd(), Point::default(), size);
        // SAFETY: `hwnd` is live. Re-enters the window procedure with no borrows held; Windows
        // applies the WM_GETMINMAXINFO limits.
        unsafe {
            SetWindowPos(
                self.hwnd(),
                ptr::null_mut(),
                0,
                0,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }

    fn position(&self) -> Point {
        let mut origin = POINT { x: 0, y: 0 };
        // SAFETY: `hwnd` is live; valid in/out pointer.
        unsafe { ClientToScreen(self.hwnd(), &mut origin) };
        Point::new(origin.x, origin.y)
    }

    fn set_position(&self, position: Point) {
        if self.state.mode.get() != WindowMode::Windowed {
            return;
        }
        let r = self
            .state
            .frame_rect(self.hwnd(), position, self.state.size.get());
        // SAFETY: `hwnd` is live. Re-enters the window procedure with no borrows held.
        unsafe {
            SetWindowPos(
                self.hwnd(),
                ptr::null_mut(),
                r.left,
                r.top,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }

    fn set_min_size(&self, size: Option<Size>) {
        self.state.min_size.set(size);
        // Re-applying the current size makes Windows enforce the new limits.
        self.set_size(self.state.size.get());
    }

    fn set_max_size(&self, size: Option<Size>) {
        self.state.max_size.set(size);
        self.set_size(self.state.size.get());
    }

    fn dpi(&self) -> u32 {
        self.state.dpi.get()
    }

    fn display(&self) -> Result<Display> {
        // SAFETY: `hwnd` is live.
        let monitor = unsafe { MonitorFromWindow(self.hwnd(), MONITOR_DEFAULTTONEAREST) };
        display::describe_monitor(monitor).ok_or(Error::from_kind(ErrorKind::NotFound))
    }

    fn set_display(&self, id: DisplayId) -> Result<()> {
        let monitor = id.0 as HMONITOR;
        if self.state.mode.get() != WindowMode::Windowed {
            return self.cover(monitor);
        }
        let info = display::monitor_info(monitor).ok_or(Error::from_kind(ErrorKind::NotFound))?;
        let mut r = empty_rect();
        // SAFETY: `hwnd` is live; valid out pointer.
        check(unsafe { GetWindowRect(self.hwnd(), &mut r) })?;
        let (x, y) = centered(&info.monitorInfo.rcWork, r.right - r.left, r.bottom - r.top);
        // SAFETY: `hwnd` is live. A DPI change on the new display re-enters with WM_DPICHANGED,
        // whose suggested rectangle keeps the window there.
        check(unsafe {
            SetWindowPos(
                self.hwnd(),
                ptr::null_mut(),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        })
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
                // SAFETY: `hwnd` is live.
                let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
                // Set first: the resize below must not apply WM_DPICHANGED's windowed rect.
                state.mode.set(mode);
                // SAFETY: `hwnd` is live. Re-enters the window procedure with no borrows held.
                unsafe { SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_POPUP | visible) as isize) };
                self.cover(monitor)?;
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

    fn is_visible(&self) -> bool {
        // SAFETY: `hwnd` is live.
        unsafe { IsWindowVisible(self.hwnd()) != 0 }
    }

    fn is_minimized(&self) -> bool {
        self.state.minimized.get()
    }

    fn is_focused(&self) -> bool {
        self.state.focused.get()
    }

    fn set_cursor_visible(&self, visible: bool) {
        self.state.cursor_visible.set(visible);
        self.state.refresh_cursor();
    }

    fn set_cursor_icon(&self, icon: CursorIcon) {
        self.state.cursor_icon.set(icon);
        self.state.refresh_cursor();
    }

    fn set_cursor_grab(&self, grab: CursorGrab) {
        self.state.grab.set(grab);
        self.state.update_clip();
        self.state.refresh_cursor();
    }

    fn cursor_grab(&self) -> CursorGrab {
        self.state.grab.get()
    }

    fn set_cursor_position(&self, position: Point) -> Result<()> {
        let mut p = POINT {
            x: position.x,
            y: position.y,
        };
        // SAFETY: `hwnd` is live; valid in/out pointer.
        check(unsafe { ClientToScreen(self.hwnd(), &mut p) })?;
        // SAFETY: no preconditions; the cursor clip still applies.
        check(unsafe { SetCursorPos(p.x, p.y) })
    }

    fn take_dropped_files(&self) -> Vec<String> {
        core::mem::take(&mut *self.state.dropped.borrow_mut())
    }
}

fn monitor_dpi(monitor: HMONITOR) -> u32 {
    let (mut x, mut y) = (0, 0);
    // SAFETY: valid out pointers; an invalid monitor just fails.
    let hr = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) };
    if hr >= 0 && x > 0 { x } else { 96 }
}

fn scale(logical: u32, dpi: u32) -> i32 {
    (logical as u64 * dpi as u64 / 96).min(i32::MAX as u64) as i32
}

/// The top-left corner that centers a `width` x `height` window in `area`, kept inside it when
/// the window fits.
fn centered(area: &RECT, width: i32, height: i32) -> (i32, i32) {
    let x = area.left + ((area.right - area.left) - width) / 2;
    let y = area.top + ((area.bottom - area.top) - height) / 2;
    (x.max(area.left), y.max(area.top))
}

fn empty_rect() -> RECT {
    RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    }
}

fn empty_placement() -> WINDOWPLACEMENT {
    let zero = POINT { x: 0, y: 0 };
    WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        flags: 0,
        showCmd: 0,
        ptMinPosition: zero,
        ptMaxPosition: zero,
        rcNormalPosition: empty_rect(),
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
