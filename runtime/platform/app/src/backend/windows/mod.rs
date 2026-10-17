//! Win32 backend: one window class for game windows, a hidden helper window for system-wide
//! notifications, a `PeekMessageW` pump, and per-window state reached from the window procedure
//! through `GWLP_USERDATA`.

mod clipboard;
mod display;
mod helper;
mod window;

use core::cell::{Cell, RefCell};
use core::mem::MaybeUninit;
use core::ptr;
use core::sync::atomic::{AtomicU8, Ordering};
use core::time::Duration;

use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;

use marlin_pal::os::windows::last_error;
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{BLACK_BRUSH, GetStockObject};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{
    CreateMemoryResourceNotification, LowMemoryResourceNotification,
    QueryMemoryResourceNotification,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, DefWindowProcW, DispatchMessageW, GWLP_USERDATA,
    GetWindowLongPtrW, IDC_ARROW, LoadCursorW, MSG, MWMO_INPUTAVAILABLE,
    MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, QS_ALLINPUT, RegisterClassExW,
    SetWindowLongPtrW, TranslateMessage, WM_NCCREATE, WM_NCDESTROY, WM_QUIT, WNDCLASSEXW, WNDPROC,
};
use windows_sys::w;

pub(crate) use window::Window;
use window::WindowState;

use crate::os::windows::{Message, MessageHook};
use crate::{Display, Error, ErrorKind, Event, EventQueue, Lifecycle, Result, WindowDesc};

const CLASS_NAME: *const u16 = w!("MarlinWindow");
const INFINITE: u32 = u32::MAX;

/// Hooks per [`App`]; one per input system is the expected use.
const MAX_HOOKS: usize = 8;

/// Reasons for [`Lifecycle::Constrained`], as bits of [`Shared::constrained`].
const CONSTRAINED_LOCKED: u8 = 1;
const CONSTRAINED_DISCONNECTED: u8 = 2;
const CONSTRAINED_DISPLAY_OFF: u8 = 4;

/// State shared by the [`App`], its helper window and the window procedure of each of its
/// windows.
pub(crate) struct Shared {
    hinstance: HINSTANCE,
    /// Filled by window procedures, moved out by `pump`. Borrowed only briefly, never across a
    /// call into Win32, because those can re-enter the window procedure.
    incoming: RefCell<EventQueue>,
    hooks: RefCell<[Option<Weak<dyn MessageHook>>; MAX_HOOKS]>,
    next_window_id: Cell<u32>,
    suspended: Cell<bool>,
    /// `CONSTRAINED_*` bits; the app is constrained while any is set.
    constrained: Cell<u8>,
}

impl Shared {
    fn push(&self, event: Event) {
        self.incoming.borrow_mut().push(event);
    }

    /// Records a system sleep or wake once, however often it is reported.
    fn set_suspended(&self, suspended: bool) {
        if self.suspended.replace(suspended) != suspended {
            self.push(if suspended {
                Event::Suspended
            } else {
                Event::Resumed
            });
        }
    }

    /// Sets or clears one reason for being constrained; reports only the transitions between
    /// none and some.
    fn set_constrained(&self, reason: u8, on: bool) {
        let before = self.constrained.get();
        let after = if on {
            before | reason
        } else {
            before & !reason
        };
        self.constrained.set(after);
        if (before == 0) != (after == 0) {
            self.push(if after != 0 {
                Event::Constrained
            } else {
                Event::Unconstrained
            });
        }
    }

    /// Offers `message` to every live hook; the first `Some` result wins.
    fn run_hooks(&self, message: &Message) -> Option<LRESULT> {
        let mut result = None;
        for i in 0..MAX_HOOKS {
            // Upgrade under a short borrow: the hook may re-enter the window procedure.
            let hook = self.hooks.borrow()[i].as_ref().and_then(Weak::upgrade);
            if let Some(hook) = hook {
                let r = hook.on_message(message);
                result = result.or(r);
            }
        }
        result
    }
}

pub(crate) struct App {
    shared: Rc<Shared>,
    helper: helper::Helper,
    /// Signaled while physical memory is low; null if unavailable.
    low_memory: HANDLE,
    memory_low: bool,
}

impl App {
    pub(crate) fn add_message_hook(&self, hook: Weak<dyn MessageHook>) -> Result<()> {
        let mut hooks = self.shared.hooks.borrow_mut();
        let slot = hooks
            .iter_mut()
            .find(|slot| slot.as_ref().is_none_or(|h| h.strong_count() == 0))
            .ok_or(Error::from_kind(ErrorKind::OutOfMemory))?;
        *slot = Some(hook);
        Ok(())
    }

    pub(crate) fn message_window(&self) -> HWND {
        self.helper.hwnd()
    }

    /// Reports [`Event::LowMemory`] when memory becomes low.
    fn poll_memory(&mut self) {
        if self.low_memory.is_null() {
            return;
        }
        let mut low = 0;
        // SAFETY: a live notification handle and a valid out pointer.
        if unsafe { QueryMemoryResourceNotification(self.low_memory, &mut low) } == 0 {
            return;
        }
        let low = low != 0;
        if low && !self.memory_low {
            self.shared.push(Event::LowMemory);
        }
        self.memory_low = low;
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if !self.low_memory.is_null() {
            // SAFETY: owned handle, closed once.
            unsafe { CloseHandle(self.low_memory) };
        }
    }
}

impl crate::contract::App for App {
    type Window = Window;

    fn new() -> Result<Self> {
        // Fails harmlessly when already set (by an earlier App or the manifest). Must precede the
        // first window, or Windows bitmap-stretches it.
        // SAFETY: no preconditions.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };

        // SAFETY: a null name returns the executable's module, which outlives the process' use.
        let hinstance = unsafe { GetModuleHandleW(ptr::null()) };
        register_class(
            &WINDOW_CLASS,
            hinstance,
            CLASS_NAME,
            Some(window_proc),
            true,
        )?;
        let shared = Rc::new(Shared {
            hinstance,
            incoming: RefCell::new(EventQueue::new()),
            hooks: RefCell::new([const { None }; MAX_HOOKS]),
            next_window_id: Cell::new(1),
            suspended: Cell::new(false),
            constrained: Cell::new(0),
        });
        let helper = helper::Helper::new(&shared)?;
        // SAFETY: no preconditions; null on failure, which disables low-memory events.
        let low_memory = unsafe { CreateMemoryResourceNotification(LowMemoryResourceNotification) };
        Ok(Self {
            shared,
            helper,
            low_memory,
            memory_low: false,
        })
    }

    fn create_window(&self, desc: &WindowDesc<'_>) -> Result<Window> {
        let id = self.shared.next_window_id.get();
        self.shared.next_window_id.set(id + 1);
        Window::new(&self.shared, crate::WindowId(id), desc)
    }

    fn pump(&mut self, timeout: Option<Duration>, out: &mut EventQueue) {
        if timeout != Some(Duration::ZERO) {
            let ms = timeout.map_or(INFINITE, |t| t.as_millis().min(INFINITE as u128 - 1) as u32);
            // SAFETY: no handles; MWMO_INPUTAVAILABLE also returns for messages already queued.
            unsafe {
                MsgWaitForMultipleObjectsEx(0, ptr::null(), ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
            };
        }

        let mut msg = MaybeUninit::<MSG>::uninit();
        // SAFETY: `msg` is a valid out pointer; a null window takes messages for every window and
        // thread message of this thread.
        while unsafe { PeekMessageW(msg.as_mut_ptr(), ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            // SAFETY: PeekMessageW returned nonzero, so it filled `msg`.
            let msg = unsafe { msg.assume_init_ref() };
            if msg.message == WM_QUIT {
                self.shared.push(Event::Quit);
                continue;
            }
            // SAFETY: `msg` came from PeekMessageW. TranslateMessage posts WM_CHAR for key messages.
            unsafe {
                TranslateMessage(msg);
                DispatchMessageW(msg);
            }
        }
        self.poll_memory();

        out.append(&mut self.shared.incoming.borrow_mut());
    }

    fn lifecycle(&self) -> Lifecycle {
        if self.shared.suspended.get() {
            Lifecycle::Suspended
        } else if self.shared.constrained.get() != 0 {
            Lifecycle::Constrained
        } else {
            Lifecycle::Running
        }
    }

    fn displays(&self, out: &mut Vec<Display>) {
        display::enumerate(out);
    }

    fn clipboard_text(&self) -> Result<Option<String>> {
        clipboard::text(self.helper.hwnd())
    }

    fn set_clipboard_text(&self, text: &str) -> Result<()> {
        clipboard::set_text(self.helper.hwnd(), text)
    }
}

/// Registration state of a window class: 0 = unregistered, 1 = registering, 2 = registered. A
/// class lives until the process exits: unregistering would race with other threads' Apps.
static WINDOW_CLASS: AtomicU8 = AtomicU8::new(0);

fn register_class(
    state: &AtomicU8,
    hinstance: HINSTANCE,
    name: *const u16,
    proc: WNDPROC,
    visible: bool,
) -> Result<()> {
    loop {
        match state.compare_exchange(0, 1, Ordering::Acquire, Ordering::Acquire) {
            Ok(_) => break,
            Err(2) => return Ok(()),
            Err(_) => core::hint::spin_loop(),
        }
    }
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        // Redraw (with the black brush) when resized, so new areas are not left garbage while a
        // swapchain catches up.
        style: if visible { CS_HREDRAW | CS_VREDRAW } else { 0 },
        lpfnWndProc: proc,
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: ptr::null_mut(),
        // SAFETY: loading a predefined system cursor has no preconditions.
        hCursor: unsafe { LoadCursorW(ptr::null_mut(), IDC_ARROW) },
        // SAFETY: stock objects need no cleanup.
        hbrBackground: unsafe { GetStockObject(BLACK_BRUSH) },
        lpszMenuName: ptr::null(),
        lpszClassName: name,
        hIconSm: ptr::null_mut(),
    };
    // SAFETY: `class` is fully initialized and its strings are static.
    if unsafe { RegisterClassExW(&class) } == 0 {
        let error = last_error();
        state.store(0, Ordering::Release);
        return Err(error);
    }
    state.store(2, Ordering::Release);
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: for WM_NCCREATE, lparam points to the CREATESTRUCTW whose lpCreateParams is the
        // WindowState passed to CreateWindowExW.
        let state = unsafe { (*(lparam as *const CREATESTRUCTW)).lpCreateParams };
        // SAFETY: `hwnd` is the window being created.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize) };
    }
    // SAFETY: `hwnd` is a live window of this class.
    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowState;
    // Messages before WM_NCCREATE (WM_GETMINMAXINFO) arrive before the state is attached.
    // SAFETY: a non-null pointer is the owning Window's boxed state, which outlives the HWND:
    // `Window::drop` destroys the window before freeing it, and WM_NCDESTROY detaches it.
    let result = match unsafe { state.as_ref() } {
        Some(state) => {
            if msg == WM_NCDESTROY {
                // SAFETY: as above.
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            }
            state.handle(hwnd, msg, wparam, lparam)
        }
        None => None,
    };
    match result {
        Some(result) => result,
        // SAFETY: forwarding the unmodified message of a live window.
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// UTF-16 up to the first NUL (or the whole slice), as chars.
fn wide_chars(wide: &[u16]) -> impl Iterator<Item = char> + '_ {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    char::decode_utf16(wide[..len].iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
}
