//! XInput, loaded at run time: `xinput1_4.dll` (Windows 8+) or `xinput9_1_0.dll`. Without
//! either, gamepads simply never connect.

use core::mem;

use marlin_pal::os::windows::from_win32;
use windows_sys::Win32::Foundation::{
    ERROR_DEVICE_NOT_CONNECTED, ERROR_SUCCESS, FreeLibrary, HMODULE,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_sys::Win32::UI::Input::XboxController::{
    XINPUT_GAMEPAD_A, XINPUT_GAMEPAD_B, XINPUT_GAMEPAD_BACK, XINPUT_GAMEPAD_DPAD_DOWN,
    XINPUT_GAMEPAD_DPAD_LEFT, XINPUT_GAMEPAD_DPAD_RIGHT, XINPUT_GAMEPAD_DPAD_UP,
    XINPUT_GAMEPAD_LEFT_SHOULDER, XINPUT_GAMEPAD_LEFT_THUMB, XINPUT_GAMEPAD_LEFT_THUMB_DEADZONE,
    XINPUT_GAMEPAD_RIGHT_SHOULDER, XINPUT_GAMEPAD_RIGHT_THUMB, XINPUT_GAMEPAD_RIGHT_THUMB_DEADZONE,
    XINPUT_GAMEPAD_START, XINPUT_GAMEPAD_TRIGGER_THRESHOLD, XINPUT_GAMEPAD_X, XINPUT_GAMEPAD_Y,
    XINPUT_STATE, XINPUT_VIBRATION,
};
use windows_sys::{s, w};

use crate::{Error, ErrorKind, GamepadButton, GamepadReading, Result, radial_deadzone};

type Proc = unsafe extern "system" fn() -> isize;
type GetState = unsafe extern "system" fn(u32, *mut XINPUT_STATE) -> u32;
type SetState = unsafe extern "system" fn(u32, *mut XINPUT_VIBRATION) -> u32;

pub(super) struct XInput {
    module: HMODULE,
    get_state: GetState,
    set_state: SetState,
}

impl XInput {
    pub(super) fn load() -> Option<Self> {
        for name in [w!("xinput1_4.dll"), w!("xinput9_1_0.dll")] {
            // SAFETY: a NUL-terminated name; System32 only, so no DLL planting.
            let module = unsafe {
                LoadLibraryExW(name, core::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32)
            };
            if module.is_null() {
                continue;
            }
            // SAFETY: `module` is loaded; the names are NUL-terminated.
            let (get, set) = unsafe {
                (
                    GetProcAddress(module, s!("XInputGetState")),
                    GetProcAddress(module, s!("XInputSetState")),
                )
            };
            if let (Some(get), Some(set)) = (get, set) {
                // SAFETY: these exports have exactly these signatures in every XInput version.
                return Some(unsafe {
                    Self {
                        module,
                        get_state: mem::transmute::<Proc, GetState>(get),
                        set_state: mem::transmute::<Proc, SetState>(set),
                    }
                });
            }
            // SAFETY: loaded above and not used afterwards.
            unsafe { FreeLibrary(module) };
        }
        None
    }

    /// Polls one slot; `None` if nothing is connected. Slow (~ms) for empty slots on some
    /// systems, so callers throttle those.
    pub(super) fn poll(&self, index: u32) -> Option<GamepadReading> {
        let mut state = XINPUT_STATE::default();
        // SAFETY: `state` is a valid out pointer; any index is accepted.
        if unsafe { (self.get_state)(index, &mut state) } != ERROR_SUCCESS {
            return None;
        }
        let pad = state.Gamepad;
        const MAP: [(u16, GamepadButton); 14] = [
            (XINPUT_GAMEPAD_A, GamepadButton::South),
            (XINPUT_GAMEPAD_B, GamepadButton::East),
            (XINPUT_GAMEPAD_X, GamepadButton::West),
            (XINPUT_GAMEPAD_Y, GamepadButton::North),
            (XINPUT_GAMEPAD_LEFT_SHOULDER, GamepadButton::LeftShoulder),
            (XINPUT_GAMEPAD_RIGHT_SHOULDER, GamepadButton::RightShoulder),
            (XINPUT_GAMEPAD_BACK, GamepadButton::Select),
            (XINPUT_GAMEPAD_START, GamepadButton::Start),
            (XINPUT_GAMEPAD_LEFT_THUMB, GamepadButton::LeftStick),
            (XINPUT_GAMEPAD_RIGHT_THUMB, GamepadButton::RightStick),
            (XINPUT_GAMEPAD_DPAD_UP, GamepadButton::DPadUp),
            (XINPUT_GAMEPAD_DPAD_DOWN, GamepadButton::DPadDown),
            (XINPUT_GAMEPAD_DPAD_LEFT, GamepadButton::DPadLeft),
            (XINPUT_GAMEPAD_DPAD_RIGHT, GamepadButton::DPadRight),
        ];
        let mut buttons = MAP
            .iter()
            .filter(|(flag, _)| pad.wButtons & flag != 0)
            .fold(0, |acc, (_, button)| acc | button.bit());

        let left_trigger = trigger(pad.bLeftTrigger);
        let right_trigger = trigger(pad.bRightTrigger);
        if left_trigger > 0.5 {
            buttons |= GamepadButton::LeftTrigger.bit();
        }
        if right_trigger > 0.5 {
            buttons |= GamepadButton::RightTrigger.bit();
        }
        Some(GamepadReading {
            buttons,
            left_stick: stick(
                pad.sThumbLX,
                pad.sThumbLY,
                XINPUT_GAMEPAD_LEFT_THUMB_DEADZONE,
            ),
            right_stick: stick(
                pad.sThumbRX,
                pad.sThumbRY,
                XINPUT_GAMEPAD_RIGHT_THUMB_DEADZONE,
            ),
            left_trigger,
            right_trigger,
        })
    }

    pub(super) fn set_rumble(&self, index: u32, low: f32, high: f32) -> Result<()> {
        let mut vibration = XINPUT_VIBRATION {
            wLeftMotorSpeed: (low * 65535.0) as u16,
            wRightMotorSpeed: (high * 65535.0) as u16,
        };
        // SAFETY: `vibration` is a valid in pointer.
        match unsafe { (self.set_state)(index, &mut vibration) } {
            ERROR_SUCCESS => Ok(()),
            ERROR_DEVICE_NOT_CONNECTED => Err(Error::new(
                ErrorKind::NotFound,
                ERROR_DEVICE_NOT_CONNECTED as i32,
            )),
            code => Err(from_win32(code)),
        }
    }
}

impl Drop for XInput {
    fn drop(&mut self) {
        // SAFETY: the function pointers die with `self`.
        unsafe { FreeLibrary(self.module) };
    }
}

fn stick(x: i16, y: i16, deadzone: u16) -> [f32; 2] {
    let axis = |v: i16| (v as f32 / 32767.0).max(-1.0);
    radial_deadzone([axis(x), axis(y)], deadzone as f32 / 32767.0)
}

fn trigger(value: u8) -> f32 {
    let threshold = XINPUT_GAMEPAD_TRIGGER_THRESHOLD as f32;
    ((value as f32 - threshold) / (255.0 - threshold)).max(0.0)
}
