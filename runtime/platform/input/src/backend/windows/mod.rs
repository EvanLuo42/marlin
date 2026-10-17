//! Win32 input: a message hook on the app's windows for keyboard, text, IME, mouse, touch and
//! pen; `WM_INPUT` for raw mouse motion and per-device keyboards and mice; XInput and HID for
//! gamepads.

mod hid;
mod ime;
mod pointer;
mod scancode;
mod xinput;

use core::cell::{Cell, RefCell};
use core::ffi::c_void;
use core::ptr;

use alloc::rc::{Rc, Weak};
use alloc::vec::Vec;

use marlin_app::os::windows::{AppExt, Message, MessageHook, WindowExt};
use marlin_app::{App, Rect, Window};
use marlin_pal::os::windows::check;
use marlin_sync::Mutex;
use windows_sys::Win32::Foundation::LRESULT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyNameTextW, MAPVK_VK_TO_VSC_EX, MapVirtualKeyW, ReleaseCapture, SetCapture, VK_NUMLOCK,
    VK_PAUSE,
};
use windows_sys::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, MOUSE_VIRTUAL_DESKTOP, RAWINPUT,
    RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT, RIDEV_DEVNOTIFY, RIDEV_REMOVE, RIM_TYPEKEYBOARD,
    RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GIDC_REMOVAL, GetSystemMetrics, RI_KEY_BREAK, RI_KEY_E0, RI_MOUSE_BUTTON_4_DOWN,
    RI_MOUSE_BUTTON_4_UP, RI_MOUSE_BUTTON_5_DOWN, RI_MOUSE_BUTTON_5_UP, RI_MOUSE_LEFT_BUTTON_DOWN,
    RI_MOUSE_LEFT_BUTTON_UP, RI_MOUSE_MIDDLE_BUTTON_DOWN, RI_MOUSE_MIDDLE_BUTTON_UP,
    RI_MOUSE_RIGHT_BUTTON_DOWN, RI_MOUSE_RIGHT_BUTTON_UP, SM_CXSCREEN, SM_CXVIRTUALSCREEN,
    SM_CYSCREEN, SM_CYVIRTUALSCREEN, WHEEL_DELTA, WM_CAPTURECHANGED, WM_CHAR, WM_IME_COMPOSITION,
    WM_IME_ENDCOMPOSITION, WM_IME_STARTCOMPOSITION, WM_INPUT, WM_INPUT_DEVICE_CHANGE, WM_KEYDOWN,
    WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_POINTERCAPTURECHANGED, WM_POINTERDOWN,
    WM_POINTERLEAVE, WM_POINTERUP, WM_POINTERUPDATE, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use crate::{
    DeviceId, Error, ErrorKind, InputEvent, InputState, Key, MAX_GAMEPADS, MouseButton, Result,
};
use xinput::XInput;

/// Raw input from mice and keyboards (HID generic desktop page, usages 2 and 6). Follow keyboard
/// focus: raw input arrives only while one of our windows is focused. DEVNOTIFY reports removals.
const RAW_DEVICES: [RAWINPUTDEVICE; 2] = [
    RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x02,
        dwFlags: RIDEV_DEVNOTIFY,
        hwndTarget: ptr::null_mut(),
    },
    RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x06,
        dwFlags: RIDEV_DEVNOTIFY,
        hwndTarget: ptr::null_mut(),
    },
];

/// XInput's user indices.
const XINPUT_USERS: usize = 4;

/// How often an empty XInput slot is polled; XInputGetState on an empty slot can take
/// milliseconds.
const EMPTY_SLOT_POLL_SECONDS: u64 = 1;

/// Who reports through a gamepad slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Slot {
    Free,
    XInput(u32),
    Hid,
}

/// Takes the lowest free slot for `owner`.
pub(super) fn allocate_slot(slots: &mut [Slot; MAX_GAMEPADS], owner: Slot) -> Option<usize> {
    let i = slots.iter().position(|s| *s == Slot::Free)?;
    slots[i] = owner;
    Some(i)
}

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
    /// Raw input device handles and the ids given to them.
    devices: RefCell<Vec<(isize, DeviceId)>>,
    next_device: Cell<u32>,
    /// Where the IME goes, re-applied when a composition starts.
    ime: Cell<Option<ime::ImeArea>>,
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

    /// The id of a raw input device handle; null (injected input) is [`DeviceId::UNKNOWN`].
    fn device(&self, handle: isize) -> DeviceId {
        if handle == 0 {
            return DeviceId::UNKNOWN;
        }
        let mut devices = self.devices.borrow_mut();
        if let Some(&(_, id)) = devices.iter().find(|(h, _)| *h == handle) {
            return id;
        }
        let id = DeviceId::from_raw(self.next_device.get());
        self.next_device.set(self.next_device.get() + 1);
        devices.push((handle, id));
        id
    }

    fn device_removed(&self, handle: isize) {
        let removed = {
            let mut devices = self.devices.borrow_mut();
            let i = devices.iter().position(|(h, _)| *h == handle);
            i.map(|i| devices.swap_remove(i).1)
        };
        if let Some(id) = removed {
            self.apply(InputEvent::DeviceRemoved(id));
        }
    }

    fn raw_input(&self, handle: HRAWINPUT) {
        let mut raw = RAWINPUT::default();
        let mut size = size_of::<RAWINPUT>() as u32;
        // SAFETY: `handle` comes from this WM_INPUT; `raw` has room for the mouse and keyboard
        // packets, the only devices registered.
        let read = unsafe {
            GetRawInputData(
                handle,
                RID_INPUT,
                (&raw mut raw).cast::<c_void>(),
                &mut size,
                size_of::<RAWINPUTHEADER>() as u32,
            )
        };
        if read == u32::MAX {
            return;
        }
        let device = self.device(raw.header.hDevice as isize);
        match raw.header.dwType {
            RIM_TYPEMOUSE => self.raw_mouse(device, &raw),
            RIM_TYPEKEYBOARD => {
                // SAFETY: the header says this is a keyboard packet.
                let keyboard = unsafe { raw.data.keyboard };
                // 0xFF marks the fake shifts some keys' escape sequences contain.
                if keyboard.VKey == 0xFF {
                    return;
                }
                let lparam = (keyboard.MakeCode as isize) << 16
                    | (((keyboard.Flags as u32 & RI_KEY_E0) != 0) as isize) << 24;
                if let Some(key) = key_from_message(keyboard.VKey, lparam) {
                    self.apply(InputEvent::DeviceKey {
                        device,
                        key,
                        down: keyboard.Flags as u32 & RI_KEY_BREAK == 0,
                    });
                }
            }
            _ => {}
        }
    }

    fn raw_mouse(&self, device: DeviceId, raw: &RAWINPUT) {
        // SAFETY: the header says this is a mouse packet.
        let mouse = unsafe { raw.data.mouse };
        // SAFETY: both union views are plain integers.
        let flags = unsafe { mouse.Anonymous.Anonymous.usButtonFlags } as u32;
        for (down, up, button) in [
            (
                RI_MOUSE_LEFT_BUTTON_DOWN,
                RI_MOUSE_LEFT_BUTTON_UP,
                MouseButton::Left,
            ),
            (
                RI_MOUSE_RIGHT_BUTTON_DOWN,
                RI_MOUSE_RIGHT_BUTTON_UP,
                MouseButton::Right,
            ),
            (
                RI_MOUSE_MIDDLE_BUTTON_DOWN,
                RI_MOUSE_MIDDLE_BUTTON_UP,
                MouseButton::Middle,
            ),
            (
                RI_MOUSE_BUTTON_4_DOWN,
                RI_MOUSE_BUTTON_4_UP,
                MouseButton::X1,
            ),
            (
                RI_MOUSE_BUTTON_5_DOWN,
                RI_MOUSE_BUTTON_5_UP,
                MouseButton::X2,
            ),
        ] {
            for (mask, is_down) in [(down, true), (up, false)] {
                if flags & mask != 0 {
                    self.apply(InputEvent::DeviceButton {
                        device,
                        button,
                        down: is_down,
                    });
                }
            }
        }
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
                device,
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
            WM_IME_STARTCOMPOSITION => {
                if let Some(ime) = self.ime.get().filter(|ime| ime.hwnd == m.hwnd) {
                    ime::place(&ime);
                }
            }
            // Read the composition without holding the state borrow across the IMM calls.
            WM_IME_COMPOSITION => {
                if let Some(composition) = ime::composition(m.hwnd, m.lparam) {
                    let composition = composition
                        .as_ref()
                        .map(|(text, cursor)| (text.as_str(), *cursor));
                    self.pending.borrow_mut().set_composition(composition);
                }
            }
            WM_IME_ENDCOMPOSITION => self.pending.borrow_mut().set_composition(None),
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
            WM_POINTERDOWN
            | WM_POINTERUPDATE
            | WM_POINTERUP
            | WM_POINTERLEAVE
            | WM_POINTERCAPTURECHANGED => {
                if let Some(event) = pointer::translate(m) {
                    self.apply(event);
                }
            }
            // DefWindowProcW must still run for WM_INPUT to free the packet.
            WM_INPUT => self.raw_input(m.lparam as HRAWINPUT),
            WM_INPUT_DEVICE_CHANGE if m.wparam as u32 == GIDC_REMOVAL => {
                self.device_removed(m.lparam)
            }
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
    /// Tick at which each idle XInput user index is polled next.
    next_poll: [u64; XINPUT_USERS],
    slots: [Slot; MAX_GAMEPADS],
    hid: hid::HidGamepads,
    ticks_per_second: u64,
}

impl crate::contract::Input for Input {
    fn new(app: &App) -> Result<Self> {
        let core = Rc::new(Core {
            pending: RefCell::new(InputState::new()),
            high_surrogate: Cell::new(0),
            captured: Cell::new(0),
            last_absolute: Cell::new(None),
            devices: RefCell::new(Vec::new()),
            next_device: Cell::new(1),
            ime: Cell::new(None),
        });
        app.add_message_hook(Rc::downgrade(&core) as Weak<dyn MessageHook>)?;
        register_raw_input()?;
        Ok(Self {
            core,
            xinput: XInput::load(),
            next_poll: [0; XINPUT_USERS],
            slots: [Slot::Free; MAX_GAMEPADS],
            hid: hid::HidGamepads::new(),
            ticks_per_second: marlin_pal::time::ticks_per_second(),
        })
    }

    fn update(&mut self, out: &mut InputState) {
        let mut pending = self.core.pending.borrow_mut();
        let now = marlin_pal::time::ticks();
        if let Some(xinput) = &self.xinput {
            for user in 0..XINPUT_USERS as u32 {
                let slot = self.slots.iter().position(|s| *s == Slot::XInput(user));
                let next = &mut self.next_poll[user as usize];
                if slot.is_none() && now < *next {
                    continue;
                }
                let reading = xinput.poll(user);
                match (slot, reading) {
                    (Some(slot), Some(reading)) => pending.set_gamepad(slot, Some(&reading)),
                    (Some(slot), None) => {
                        pending.set_gamepad(slot, None);
                        self.slots[slot] = Slot::Free;
                        *next = now + EMPTY_SLOT_POLL_SECONDS * self.ticks_per_second;
                    }
                    (None, Some(reading)) => {
                        if let Some(slot) = allocate_slot(&mut self.slots, Slot::XInput(user)) {
                            pending.set_gamepad(slot, Some(&reading));
                        }
                    }
                    (None, None) => *next = now + EMPTY_SLOT_POLL_SECONDS * self.ticks_per_second,
                }
            }
        }
        self.hid
            .update(now, self.ticks_per_second, &mut pending, &mut self.slots);
        *out = *pending;
        pending.begin_frame();
    }

    fn set_rumble(&mut self, gamepad: usize, low: f32, high: f32) -> Result<()> {
        match (self.slots[gamepad], &self.xinput) {
            (Slot::XInput(user), Some(xinput)) => xinput.set_rumble(user, low, high),
            (Slot::Hid, _) => self.hid_pad(gamepad)?.set_rumble(low, high),
            _ => Err(Error::from_kind(ErrorKind::NotFound)),
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

    fn set_text_input(&mut self, window: &Window, area: Option<Rect>) -> Result<()> {
        let hwnd = window.hwnd();
        ime::set_text_input(hwnd, area)?;
        let current = self.core.ime.get();
        self.core.ime.set(match area {
            Some(area) => Some(ime::ImeArea { hwnd, area }),
            None => current.filter(|ime| ime.hwnd != hwnd),
        });
        if area.is_none() {
            self.core.pending.borrow_mut().set_composition(None);
        }
        Ok(())
    }
}

impl Input {
    /// The HID pad in `slot`, or `NotFound`.
    fn hid_pad(&mut self, slot: usize) -> Result<&mut hid::Pad> {
        self.hid
            .pad(slot)
            .ok_or(Error::from_kind(ErrorKind::NotFound))
    }

    /// The HID pad in `slot`; `Unsupported` for an XInput pad, `NotFound` for an empty slot.
    #[cfg(any(
        marlin_ext = "gyro",
        marlin_ext = "adaptive_triggers",
        marlin_ext = "haptics"
    ))]
    fn ext_pad(&mut self, slot: usize) -> Result<&mut hid::Pad> {
        match self.slots[slot] {
            Slot::XInput(_) => Err(Error::from_kind(ErrorKind::Unsupported)),
            _ => self.hid_pad(slot),
        }
    }
}

#[cfg(marlin_ext = "gyro")]
impl crate::contract::ext::Gyro for Input {
    fn motion_available(&self) -> bool {
        true
    }

    fn set_motion_enabled(&mut self, gamepad: usize, enabled: bool) -> Result<()> {
        self.ext_pad(gamepad)?.set_motion_enabled(enabled)
    }
}

#[cfg(marlin_ext = "adaptive_triggers")]
impl crate::contract::ext::AdaptiveTriggers for Input {
    fn adaptive_triggers_available(&self) -> bool {
        true
    }

    fn set_trigger_effect(
        &mut self,
        gamepad: usize,
        trigger: crate::Trigger,
        effect: &crate::TriggerEffect,
    ) -> Result<()> {
        self.ext_pad(gamepad)?.set_trigger_effect(trigger, effect)
    }
}

#[cfg(marlin_ext = "haptics")]
impl crate::contract::ext::Haptics for Input {
    fn haptics_available(&self) -> bool {
        true
    }

    fn set_haptics(&mut self, gamepad: usize, tones: Option<[crate::HapticTone; 2]>) -> Result<()> {
        self.ext_pad(gamepad)?.set_haptics(tones)
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        unregister_raw_input();
    }
}

/// Live [`Input`]s. Raw input is registered for the whole process, not per window or thread:
/// the first `Input` registers and the last removes, so dropping one leaves the others' raw input
/// flowing.
static RAW_INPUT_USERS: Mutex<usize> = Mutex::new(0);

fn register_raw_input() -> Result<()> {
    let mut users = RAW_INPUT_USERS.lock();
    if *users == 0 {
        // SAFETY: valid device descriptions.
        check(unsafe {
            RegisterRawInputDevices(
                RAW_DEVICES.as_ptr(),
                RAW_DEVICES.len() as u32,
                size_of::<RAWINPUTDEVICE>() as u32,
            )
        })?;
    }
    *users += 1;
    Ok(())
}

fn unregister_raw_input() {
    let mut users = RAW_INPUT_USERS.lock();
    *users -= 1;
    if *users == 0 {
        let devices = RAW_DEVICES.map(|d| RAWINPUTDEVICE {
            dwFlags: RIDEV_REMOVE,
            ..d
        });
        // SAFETY: valid device descriptions, registered by `register_raw_input`.
        unsafe {
            RegisterRawInputDevices(
                devices.as_ptr(),
                devices.len() as u32,
                size_of::<RAWINPUTDEVICE>() as u32,
            )
        };
    }
}
