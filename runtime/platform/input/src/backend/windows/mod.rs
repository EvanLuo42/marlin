//! Win32 input: a message hook on the app's windows for keyboard, text and mouse, `WM_INPUT` for
//! raw mouse motion, XInput for gamepads.

mod scancode;
mod xinput;

use core::cell::{Cell, RefCell};
use core::ffi::c_void;
use core::ptr;

use alloc::rc::{Rc, Weak};

use marlin_app::App;
use marlin_app::os::windows::{AppExt, Message, MessageHook};
use marlin_pal::os::windows::check;
use windows_sys::Win32::Foundation::LRESULT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyNameTextW, MAPVK_VK_TO_VSC_EX, MapVirtualKeyW, ReleaseCapture, SetCapture, VK_NUMLOCK,
    VK_PAUSE,
};
use windows_sys::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, MOUSE_VIRTUAL_DESKTOP, RAWINPUT,
    RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT, RIDEV_REMOVE, RIM_TYPEMOUSE,
    RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN,
    WHEEL_DELTA, WM_CAPTURECHANGED, WM_CHAR, WM_INPUT, WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN,
    WM_XBUTTONUP,
};

use crate::{Error, ErrorKind, InputEvent, InputState, Key, MAX_GAMEPADS, MouseButton, Result};
use xinput::XInput;

/// HID generic desktop page, mouse usage.
const MOUSE_DEVICE: RAWINPUTDEVICE = RAWINPUTDEVICE {
    usUsagePage: 0x01,
    usUsage: 0x02,
    dwFlags: 0,
    // Follow keyboard focus: raw input arrives only while one of our windows is focused.
    hwndTarget: ptr::null_mut(),
};

/// How often an empty gamepad slot is polled; XInputGetState on an empty slot can take
/// milliseconds.
const EMPTY_SLOT_POLL_SECONDS: u64 = 1;

/// State reached from the message hook. Interior mutability, borrowed only briefly: the hook can
/// be re-entered by the Win32 calls it makes.
struct Core {
    pending: RefCell<InputState>,
    /// A high surrogate from `WM_CHAR`, waiting for its low half.
    high_surrogate: Cell<u16>,
    /// Mouse buttons held while the mouse is captured.
    captured: Cell<u8>,
    /// Last absolute raw position (tablets, remote desktop), to turn it into motion.
    last_absolute: Cell<Option<(i32, i32)>>,
}

impl Core {
    fn apply(&self, event: InputEvent) {
        self.pending.borrow_mut().apply(event);
    }

    fn mouse_button(&self, message: &Message, button: MouseButton, down: bool) {
        self.apply(InputEvent::MouseButton { button, down });
        // Capture while any button is held, so drags that leave the window still end.
        let bit = 1 << button as u8;
        let before = self.captured.get();
        let after = if down { before | bit } else { before & !bit };
        self.captured.set(after);
        // SAFETY: `hwnd` is the live window receiving this message, on this thread. These calls
        // may re-enter the hook (WM_CAPTURECHANGED); no borrow is held.
        unsafe {
            if before == 0 && after != 0 {
                SetCapture(message.hwnd);
            } else if before != 0 && after == 0 {
                ReleaseCapture();
            }
        }
    }

    fn raw_input(&self, handle: HRAWINPUT) {
        let mut raw = RAWINPUT::default();
        let mut size = size_of::<RAWINPUT>() as u32;
        // SAFETY: `handle` comes from this WM_INPUT; `raw` has room for a mouse packet, the only
        // device registered.
        let read = unsafe {
            GetRawInputData(
                handle,
                RID_INPUT,
                (&raw mut raw).cast::<c_void>(),
                &mut size,
                size_of::<RAWINPUTHEADER>() as u32,
            )
        };
        if read == u32::MAX || raw.header.dwType != RIM_TYPEMOUSE {
            return;
        }
        // SAFETY: the header says this is a mouse packet.
        let mouse = unsafe { raw.data.mouse };
        let (x, y) = if mouse.usFlags & MOUSE_MOVE_ABSOLUTE != 0 {
            // Absolute devices report 0..=65535 over the (virtual) screen.
            let (w, h) = if mouse.usFlags & MOUSE_VIRTUAL_DESKTOP != 0 {
                (SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN)
            } else {
                (SM_CXSCREEN, SM_CYSCREEN)
            };
            // SAFETY: no preconditions.
            let (w, h) = unsafe { (GetSystemMetrics(w), GetSystemMetrics(h)) };
            let position = (
                (mouse.lLastX as i64 * w as i64 / 65535) as i32,
                (mouse.lLastY as i64 * h as i64 / 65535) as i32,
            );
            match self.last_absolute.replace(Some(position)) {
                Some(last) => (position.0 - last.0, position.1 - last.1),
                None => return,
            }
        } else {
            (mouse.lLastX, mouse.lLastY)
        };
        if x != 0 || y != 0 {
            self.apply(InputEvent::RawMotion {
                x: x as f32,
                y: y as f32,
            });
        }
    }

    fn text(&self, unit: u16) {
        let c = match unit {
            0xD800..=0xDBFF => {
                self.high_surrogate.set(unit);
                return;
            }
            0xDC00..=0xDFFF => {
                let high = self.high_surrogate.replace(0);
                if high == 0 {
                    return;
                }
                0x10000 + ((high as u32 - 0xD800) << 10) + (unit as u32 - 0xDC00)
            }
            _ => unit as u32,
        };
        if let Some(c) = char::from_u32(c) {
            self.apply(InputEvent::Text(c));
        }
    }
}

impl MessageHook for Core {
    fn on_message(&self, m: &Message) -> Option<LRESULT> {
        let x = (m.lparam & 0xFFFF) as i16 as f32;
        let y = ((m.lparam >> 16) & 0xFFFF) as i16 as f32;
        let wheel = ((m.wparam >> 16) & 0xFFFF) as i16 as f32 / WHEEL_DELTA as f32;
        match m.msg {
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
                if let Some(key) = key_from_message(m.wparam as u16, m.lparam) {
                    self.apply(InputEvent::Key {
                        key,
                        down: matches!(m.msg, WM_KEYDOWN | WM_SYSKEYDOWN),
                        repeat: m.lparam & (1 << 30) != 0,
                    });
                }
            }
            WM_CHAR => self.text(m.wparam as u16),
            WM_MOUSEMOVE => self.apply(InputEvent::MouseMoved {
                window: m.window,
                x,
                y,
            }),
            WM_LBUTTONDOWN => self.mouse_button(m, MouseButton::Left, true),
            WM_LBUTTONUP => self.mouse_button(m, MouseButton::Left, false),
            WM_RBUTTONDOWN => self.mouse_button(m, MouseButton::Right, true),
            WM_RBUTTONUP => self.mouse_button(m, MouseButton::Right, false),
            WM_MBUTTONDOWN => self.mouse_button(m, MouseButton::Middle, true),
            WM_MBUTTONUP => self.mouse_button(m, MouseButton::Middle, false),
            WM_XBUTTONDOWN | WM_XBUTTONUP => {
                let button = if (m.wparam >> 16) & 0xFFFF == 1 {
                    MouseButton::X1
                } else {
                    MouseButton::X2
                };
                self.mouse_button(m, button, m.msg == WM_XBUTTONDOWN);
            }
            WM_MOUSEWHEEL => self.apply(InputEvent::Wheel { x: 0.0, y: wheel }),
            WM_MOUSEHWHEEL => self.apply(InputEvent::Wheel { x: wheel, y: 0.0 }),
            // DefWindowProcW must still run for WM_INPUT to free the packet.
            WM_INPUT => self.raw_input(m.lparam as HRAWINPUT),
            WM_CAPTURECHANGED => self.captured.set(0),
            WM_KILLFOCUS => {
                self.high_surrogate.set(0);
                self.last_absolute.set(None);
                self.apply(InputEvent::FocusLost);
            }
            _ => {}
        }
        None
    }
}

/// The physical key of a key message, from its scan code.
fn key_from_message(vk: u16, lparam: isize) -> Option<Key> {
    // Pause and Num Lock share scan code 0x45 in key messages.
    match vk {
        VK_PAUSE => return Some(Key::PAUSE),
        VK_NUMLOCK => return Some(Key::NUM_LOCK),
        _ => {}
    }
    let mut code = ((lparam >> 16) & 0xFF) as u32;
    let mut extended = lparam & (1 << 24) != 0;
    if code == 0 {
        // Synthesized input (SendInput with only a virtual key) has no scan code.
        // SAFETY: no preconditions.
        let mapped = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC_EX) };
        code = mapped & 0xFF;
        extended = mapped & 0xFF00 == 0xE000;
    }
    scancode::to_key(code as u8, extended)
}

pub(crate) struct Input {
    core: Rc<Core>,
    xinput: Option<XInput>,
    /// Tick at which each empty slot is polled next.
    next_poll: [u64; MAX_GAMEPADS],
    ticks_per_second: u64,
}

impl crate::contract::Input for Input {
    fn new(app: &App) -> Result<Self> {
        let core = Rc::new(Core {
            pending: RefCell::new(InputState::new()),
            high_surrogate: Cell::new(0),
            captured: Cell::new(0),
            last_absolute: Cell::new(None),
        });
        app.add_message_hook(Rc::downgrade(&core) as Weak<dyn MessageHook>)?;
        // SAFETY: one valid device description.
        check(unsafe {
            RegisterRawInputDevices(&MOUSE_DEVICE, 1, size_of::<RAWINPUTDEVICE>() as u32)
        })?;
        Ok(Self {
            core,
            xinput: XInput::load(),
            next_poll: [0; MAX_GAMEPADS],
            ticks_per_second: marlin_pal::time::ticks_per_second(),
        })
    }

    fn update(&mut self, out: &mut InputState) {
        let mut pending = self.core.pending.borrow_mut();
        if let Some(xinput) = &self.xinput {
            let now = marlin_pal::time::ticks();
            for (i, next) in self.next_poll.iter_mut().enumerate() {
                if !pending.gamepad(i).is_connected() && now < *next {
                    continue;
                }
                let reading = xinput.poll(i as u32);
                if reading.is_none() {
                    *next = now + EMPTY_SLOT_POLL_SECONDS * self.ticks_per_second;
                }
                pending.set_gamepad(i, reading.as_ref());
            }
        }
        *out = *pending;
        pending.begin_frame();
    }

    fn set_rumble(&mut self, gamepad: usize, low: f32, high: f32) -> Result<()> {
        match &self.xinput {
            Some(xinput) => xinput.set_rumble(gamepad as u32, low, high),
            None => Err(Error::from_kind(ErrorKind::NotFound)),
        }
    }

    fn key_label<'b>(&self, key: Key, buf: &'b mut [u8]) -> Option<&'b str> {
        let (code, extended) = scancode::from_key(key)?;
        let lparam = (code as i32) << 16 | (extended as i32) << 24;
        let mut wide = [0u16; 64];
        // SAFETY: `wide` holds 64 units; the call writes at most that many including the NUL.
        let len = unsafe { GetKeyNameTextW(lparam, wide.as_mut_ptr(), wide.len() as i32) };
        if len <= 0 {
            return None;
        }
        let mut n = 0;
        for c in char::decode_utf16(wide[..len as usize].iter().copied()) {
            let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
            if n + c.len_utf8() > buf.len() {
                break;
            }
            n += c.encode_utf8(&mut buf[n..]).len();
        }
        core::str::from_utf8(&buf[..n]).ok()
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        let device = RAWINPUTDEVICE {
            dwFlags: RIDEV_REMOVE,
            ..MOUSE_DEVICE
        };
        // SAFETY: one valid device description; removing an unregistered device is harmless.
        unsafe { RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>() as u32) };
    }
}
