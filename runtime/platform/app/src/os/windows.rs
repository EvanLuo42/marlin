//! Win32 extensions: window handles and hooks that observe every window message.
//!
//! `marlin-input` uses a hook to see keyboard, mouse and raw input messages as the pump dispatches
//! them, so input never has to fit through the app's event queue.

use alloc::rc::Weak;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

use crate::{App, Result, Window, WindowId};

/// A window message, as the window procedure received it.
#[derive(Clone, Copy, Debug)]
pub struct Message {
    pub window: WindowId,
    pub hwnd: HWND,
    pub msg: u32,
    pub wparam: WPARAM,
    pub lparam: LPARAM,
}

/// Observes the messages of every window of an [`App`]. Called on the pump thread, after the app
/// has updated its own window state and before `DefWindowProcW`.
pub trait MessageHook {
    /// Returns `Some(result)` to answer the message instead of `DefWindowProcW`; the app's own
    /// answer takes precedence. Observers return `None`.
    ///
    /// May be re-entered: Win32 calls made here (`SetCapture`, ...) can send messages
    /// synchronously, so do not hold a borrow across them.
    fn on_message(&self, message: &Message) -> Option<LRESULT>;
}

pub trait AppExt {
    /// Registers `hook` for every current and future window. Held weakly: dropping the last `Rc`
    /// unregisters it. Fails with `OutOfMemory` when all slots hold live hooks.
    fn add_message_hook(&self, hook: Weak<dyn MessageHook>) -> Result<()>;
}

impl AppExt for App {
    fn add_message_hook(&self, hook: Weak<dyn MessageHook>) -> Result<()> {
        self.inner.add_message_hook(hook)
    }
}

pub trait WindowExt {
    fn hwnd(&self) -> HWND;
}

impl WindowExt for Window {
    fn hwnd(&self) -> HWND {
        self.inner.hwnd()
    }
}
