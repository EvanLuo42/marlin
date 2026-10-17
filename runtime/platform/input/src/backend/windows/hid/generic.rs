//! HID gamepads without a driver of their own, read through the report descriptor (`HidP_*`).
//!
//! There is no standard button layout, so the mapping is a heuristic that fits most
//! DirectInput-era pads: X/Y are the left stick; Rx/Ry the right stick with Z/Rz as analog
//! triggers when present, otherwise Z/Rz the right stick; the hat is the d-pad; buttons 1–13 are
//! South, East, West, North, the shoulders, the digital triggers, Select, Start, the stick clicks
//! and Home. A per-device mapping database would replace this.

use core::mem;

use alloc::vec::Vec;

use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HIDP_CAPS, HIDP_STATUS_SUCCESS, HIDP_VALUE_CAPS, HidD_FreePreparsedData, HidD_GetPreparsedData,
    HidP_GetCaps, HidP_GetUsageValue, HidP_GetUsages, HidP_GetValueCaps, HidP_Input,
    HidP_MaxUsageListLength, PHIDP_PREPARSED_DATA,
};
use windows_sys::Win32::Foundation::HANDLE;

use crate::hid::{STICK_DEADZONE, hat};
use crate::{GamepadButton, GamepadKind, GamepadReading, radial_deadzone};

const PAGE_GENERIC_DESKTOP: u16 = 0x01;
const PAGE_BUTTON: u16 = 0x09;
/// Generic desktop usages X, Y, Z, Rx, Ry, Rz, then the hat switch.
const AXES: [u16; 6] = [0x30, 0x31, 0x32, 0x33, 0x34, 0x35];
const HAT: u16 = 0x39;
const BUTTONS: [GamepadButton; 13] = [
    GamepadButton::South,
    GamepadButton::East,
    GamepadButton::West,
    GamepadButton::North,
    GamepadButton::LeftShoulder,
    GamepadButton::RightShoulder,
    GamepadButton::LeftTrigger,
    GamepadButton::RightTrigger,
    GamepadButton::Select,
    GamepadButton::Start,
    GamepadButton::LeftStick,
    GamepadButton::RightStick,
    GamepadButton::Home,
];

/// A value's logical range and size, from the report descriptor.
#[derive(Clone, Copy)]
struct Range {
    min: i32,
    max: i32,
    bits: u16,
}

impl Range {
    /// `raw` in [0, 1]. Signed ranges arrive as raw bits and are sign-extended first.
    fn normalize(&self, raw: u32) -> f32 {
        let value = if self.min < 0 && self.bits > 0 && self.bits < 32 {
            let shift = 32 - self.bits as u32;
            ((raw << shift) as i32) >> shift
        } else {
            raw as i32
        };
        let span = self.max as f32 - self.min as f32;
        if span <= 0.0 {
            return 0.5;
        }
        ((value as f32 - self.min as f32) / span).clamp(0.0, 1.0)
    }
}

pub(super) struct Generic {
    preparsed: PHIDP_PREPARSED_DATA,
    axes: [Option<Range>; 6],
    hat: Option<Range>,
    usages: Vec<u16>,
}

impl Generic {
    pub(super) fn new(file: HANDLE) -> Option<Self> {
        let mut preparsed: PHIDP_PREPARSED_DATA = 0;
        // SAFETY: live handle, valid out pointer.
        if !unsafe { HidD_GetPreparsedData(file, &mut preparsed) } {
            return None;
        }
        // From here `Drop` frees `preparsed`.
        let mut generic = Self {
            preparsed,
            axes: [None; 6],
            hat: None,
            usages: Vec::new(),
        };
        // SAFETY: all-zero is a valid HIDP_CAPS.
        let mut caps: HIDP_CAPS = unsafe { mem::zeroed() };
        // SAFETY: valid preparsed data and out pointer.
        if unsafe { HidP_GetCaps(preparsed, &mut caps) } != HIDP_STATUS_SUCCESS {
            return None;
        }
        let mut count = caps.NumberInputValueCaps;
        // SAFETY: all-zero is a valid HIDP_VALUE_CAPS.
        let mut values: Vec<HIDP_VALUE_CAPS> =
            (0..count).map(|_| unsafe { mem::zeroed() }).collect();
        if count > 0 {
            // SAFETY: the array holds `count` entries.
            let status = unsafe {
                HidP_GetValueCaps(HidP_Input, values.as_mut_ptr(), &mut count, preparsed)
            };
            if status != HIDP_STATUS_SUCCESS {
                return None;
            }
        }
        for cap in &values[..count as usize] {
            if cap.UsagePage != PAGE_GENERIC_DESKTOP {
                continue;
            }
            // SAFETY: `IsRange` says which union member is valid.
            let (first, last) = unsafe {
                if cap.IsRange {
                    (cap.Anonymous.Range.UsageMin, cap.Anonymous.Range.UsageMax)
                } else {
                    (cap.Anonymous.NotRange.Usage, cap.Anonymous.NotRange.Usage)
                }
            };
            let range = Range {
                min: cap.LogicalMin,
                max: cap.LogicalMax,
                bits: cap.BitSize,
            };
            // Some descriptors declare an unsigned maximum that overflowed into a negative one.
            let range = if range.max < range.min && range.bits < 32 {
                Range {
                    min: 0,
                    max: ((1u64 << range.bits) - 1) as i32,
                    ..range
                }
            } else {
                range
            };
            for usage in first..=last {
                if let Some(i) = AXES.iter().position(|&a| a == usage) {
                    generic.axes[i] = Some(range);
                } else if usage == HAT {
                    generic.hat = Some(range);
                }
            }
        }
        // SAFETY: valid preparsed data.
        let max = unsafe { HidP_MaxUsageListLength(HidP_Input, PAGE_BUTTON, preparsed) };
        generic.usages = alloc::vec![0; max as usize];
        // A gamepad has at least a stick or some buttons.
        (generic.axes[0].is_some() || max > 0).then_some(generic)
    }

    pub(super) fn parse(&mut self, report: &mut [u8]) -> Option<GamepadReading> {
        let value = |usage: u16| {
            let mut v = 0;
            // SAFETY: valid preparsed data; the report buffer holds `report.len()` bytes.
            let status = unsafe {
                HidP_GetUsageValue(
                    HidP_Input,
                    PAGE_GENERIC_DESKTOP,
                    0,
                    usage,
                    &mut v,
                    self.preparsed,
                    report.as_ptr(),
                    report.len() as u32,
                )
            };
            (status == HIDP_STATUS_SUCCESS).then_some(v)
        };
        let axis = |i: usize| {
            self.axes[i]
                .zip(value(AXES[i]))
                .map(|(range, raw)| range.normalize(raw))
        };
        let [x, y, z, rx, ry, rz] = [0, 1, 2, 3, 4, 5].map(axis);
        let hat_bits = self.hat.zip(value(HAT)).map_or(0, |(range, raw)| {
            let position = raw as i64 - range.min as i64;
            match range.max as i64 - range.min as i64 {
                // A four-way hat: up, right, down, left.
                3 => hat((position * 2) as u8),
                7 => hat(position as u8),
                _ => 0,
            }
        });

        let mut count = self.usages.len() as u32;
        // SAFETY: valid preparsed data; `usages` holds `count` entries; the report buffer is
        // mutable as the API declares, though it is only read.
        let status = unsafe {
            HidP_GetUsages(
                HidP_Input,
                PAGE_BUTTON,
                0,
                self.usages.as_mut_ptr(),
                &mut count,
                self.preparsed,
                report.as_mut_ptr(),
                report.len() as u32,
            )
        };
        let mut buttons = hat_bits;
        if status == HIDP_STATUS_SUCCESS {
            for &usage in &self.usages[..count as usize] {
                if let Some(button) = (usage as usize).checked_sub(1).and_then(|i| BUTTONS.get(i)) {
                    buttons |= button.bit();
                }
            }
        }

        // Centered [0, 1] values as [-1, 1]; HID +y is down, ours is up.
        let signed = |v: Option<f32>| v.map_or(0.0, |v| v * 2.0 - 1.0);
        let stick = |h: Option<f32>, v: Option<f32>| {
            radial_deadzone([signed(h), -signed(v)], STICK_DEADZONE)
        };
        let (right_stick, left_trigger, right_trigger) = if rx.is_some() && ry.is_some() {
            (stick(rx, ry), z.unwrap_or(0.0), rz.unwrap_or(0.0))
        } else {
            let digital = |b: GamepadButton| if buttons & b.bit() != 0 { 1.0 } else { 0.0 };
            (
                stick(z, rz),
                digital(GamepadButton::LeftTrigger),
                digital(GamepadButton::RightTrigger),
            )
        };
        if left_trigger > 0.5 {
            buttons |= GamepadButton::LeftTrigger.bit();
        }
        if right_trigger > 0.5 {
            buttons |= GamepadButton::RightTrigger.bit();
        }
        Some(GamepadReading {
            buttons,
            left_stick: stick(x, y),
            right_stick,
            left_trigger,
            right_trigger,
            kind: GamepadKind::Generic,
            motion: None,
        })
    }
}

impl Drop for Generic {
    fn drop(&mut self) {
        // SAFETY: from HidD_GetPreparsedData, freed once.
        unsafe { HidD_FreePreparsedData(self.preparsed) };
    }
}
