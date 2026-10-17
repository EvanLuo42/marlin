//! A hidden top-level window that receives what is not about one game window: power (sleep,
//! display off), session (lock, remote disconnect) and display-configuration broadcasts. It also
//! owns the clipboard while the app reads or writes it.

use core::ptr;
use core::sync::atomic::AtomicU8;

use alloc::rc::Rc;

use marlin_pal::os::windows::last_error;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Power::{
    HPOWERNOTIFY, POWERBROADCAST_SETTING, RegisterPowerSettingNotification,
    UnregisterPowerSettingNotification,
};
use windows_sys::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows_sys::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW, DestroyWindow,
    GWLP_USERDATA, GetWindowLongPtrW, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND,
    PBT_POWERSETTINGCHANGE, SetWindowLongPtrW, WM_DISPLAYCHANGE, WM_NCCREATE, WM_NCDESTROY,
    WM_POWERBROADCAST, WM_WTSSESSION_CHANGE, WS_EX_TOOLWINDOW, WS_POPUP, WTS_CONSOLE_CONNECT,
    WTS_CONSOLE_DISCONNECT, WTS_REMOTE_CONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK,
    WTS_SESSION_UNLOCK,
};
use windows_sys::w;

use super::{
    CONSTRAINED_DISCONNECTED, CONSTRAINED_DISPLAY_OFF, CONSTRAINED_LOCKED, Shared, register_class,
};
use crate::{Event, Result};

const CLASS_NAME: *const u16 = w!("MarlinAppHelper");
static CLASS: AtomicU8 = AtomicU8::new(0);

pub(super) struct Helper {
    hwnd: HWND,
    /// Null when the registration failed (display-off events are then never sent).
    display_state: HPOWERNOTIFY,
    session: bool,
    /// Keeps the state the window procedure points to alive until the window is destroyed.
    _shared: Rc<Shared>,
}

impl Helper {
    pub(super) fn new(shared: &Rc<Shared>) -> Result<Self> {
        register_class(
            &CLASS,
            shared.hinstance,
            CLASS_NAME,
            Some(helper_proc),
            false,
        )?;
        // Top-level (not message-only), so it receives broadcasts; never shown, and a tool window
        // so it has no taskbar button even if something shows it.
        // SAFETY: the class is registered; the Shared pointer stays valid for the window's life
        // because `_shared` holds it and `drop` destroys the window first.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CLASS_NAME,
                ptr::null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                shared.hinstance,
                Rc::as_ptr(shared).cast(),
            )
        };
        if hwnd.is_null() {
            return Err(last_error());
        }
        // Both registrations are best effort: without them the app just never becomes constrained
        // for that reason.
        // SAFETY: `hwnd` is live; the GUID is a static.
        let display_state = unsafe {
            RegisterPowerSettingNotification(
                hwnd,
                &GUID_CONSOLE_DISPLAY_STATE,
                DEVICE_NOTIFY_WINDOW_HANDLE,
            )
        };
        // SAFETY: `hwnd` is live.
        let session = unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) } != 0;
        Ok(Self {
            hwnd,
            display_state,
            session,
            _shared: shared.clone(),
        })
    }

    pub(super) fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        // SAFETY: undoing this helper's own registrations on its live window, then destroying it
        // on its thread.
        unsafe {
            if self.display_state != 0 {
                UnregisterPowerSettingNotification(self.display_state);
            }
            if self.session {
                WTSUnRegisterSessionNotification(self.hwnd);
            }
            DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn helper_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: for WM_NCCREATE, lparam points to the CREATESTRUCTW whose lpCreateParams is the
        // Shared passed to CreateWindowExW.
        let shared = unsafe { (*(lparam as *const CREATESTRUCTW)).lpCreateParams };
        // SAFETY: `hwnd` is the window being created.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, shared as isize) };
    }
    // SAFETY: `hwnd` is a live window of this class.
    let shared = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Shared;
    // SAFETY: a non-null pointer is the Shared kept alive by the owning Helper (see `new`).
    if let Some(shared) = unsafe { shared.as_ref() } {
        if msg == WM_NCDESTROY {
            // SAFETY: as above.
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
        }
        if let Some(result) = handle(shared, msg, wparam, lparam) {
            return result;
        }
    }
    // SAFETY: forwarding the unmodified message of a live window.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn handle(shared: &Shared, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_POWERBROADCAST => {
            match wparam as u32 {
                PBT_APMSUSPEND => shared.set_suspended(true),
                PBT_APMRESUMEAUTOMATIC => shared.set_suspended(false),
                PBT_POWERSETTINGCHANGE => {
                    // SAFETY: for PBT_POWERSETTINGCHANGE, lparam points to a POWERBROADCAST_SETTING
                    // followed by `DataLength` bytes.
                    let setting = unsafe { &*(lparam as *const POWERBROADCAST_SETTING) };
                    if same_guid(&setting.PowerSetting, &GUID_CONSOLE_DISPLAY_STATE)
                        && setting.DataLength >= 4
                    {
                        // SAFETY: at least 4 data bytes; possibly unaligned.
                        let state =
                            unsafe { ptr::read_unaligned(setting.Data.as_ptr().cast::<u32>()) };
                        // 0 = off, 1 = on, 2 = dimmed (still visible).
                        shared.set_constrained(CONSTRAINED_DISPLAY_OFF, state == 0);
                    }
                }
                _ => {}
            }
            Some(1)
        }
        WM_WTSSESSION_CHANGE => {
            match wparam as u32 {
                WTS_SESSION_LOCK => shared.set_constrained(CONSTRAINED_LOCKED, true),
                WTS_SESSION_UNLOCK => shared.set_constrained(CONSTRAINED_LOCKED, false),
                WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT => {
                    shared.set_constrained(CONSTRAINED_DISCONNECTED, true)
                }
                WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT => {
                    shared.set_constrained(CONSTRAINED_DISCONNECTED, false)
                }
                _ => {}
            }
            Some(0)
        }
        WM_DISPLAYCHANGE => {
            shared.push(Event::DisplaysChanged);
            Some(0)
        }
        _ => None,
    }
}

fn same_guid(a: &windows_sys::core::GUID, b: &windows_sys::core::GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}
