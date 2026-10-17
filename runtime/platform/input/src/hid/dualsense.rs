//! DualSense (and DualSense Edge).
//!
//! Input: report 0x01 over USB (64 bytes). Over Bluetooth the controller starts with a reduced
//! 0x01 report without sensors and switches to the full 0x31 report (78 bytes, CRC-checked) once
//! the calibration feature report has been read. Output: report 0x02 over USB, 0x31 with a
//! sequence tag and CRC over Bluetooth.

use super::{
    Calibration, Feedback, STICK_DEADZONE, Transport, axis_u8, crc_ok, le16, playstation_face,
    playstation_shoulders, seal, trigger_bits,
};
use crate::{GamepadButton, GamepadKind, GamepadReading, TriggerEffect, radial_deadzone};

/// The feature report holding the IMU calibration; reading it over Bluetooth also enables the
/// full input report.
pub const CALIBRATION_REPORT: u8 = 0x05;
pub const CALIBRATION_LEN: usize = 41;
/// Bytes [`output`] writes, per transport. HID APIs may want the report padded to the length
/// the device declares; the padding stays zero.
pub const OUTPUT_USB_LEN: usize = 48;
pub const OUTPUT_BLUETOOTH_LEN: usize = 78;

const INPUT_BLUETOOTH_LEN: usize = 78;

/// Parses the calibration feature report (id included).
pub fn calibration(report: &[u8]) -> Calibration {
    if report.len() < CALIBRATION_LEN || report[0] != CALIBRATION_REPORT {
        return Calibration::NOMINAL;
    }
    let w = |at| le16(report, at);
    Calibration::from_words(
        [w(1), w(3), w(5)],
        [(w(7), w(9)), (w(11), w(13)), (w(15), w(17))],
        (w(19), w(21)),
        [(w(23), w(25)), (w(27), w(29)), (w(31), w(33))],
    )
}

/// Parses an input report. `None` for other reports and Bluetooth reports with a bad CRC.
pub fn parse(report: &[u8], transport: Transport, cal: &Calibration) -> Option<GamepadReading> {
    let common = match (transport, report.first()?) {
        (Transport::Usb, 0x01) if report.len() >= 64 => &report[1..],
        (Transport::Bluetooth, 0x31) if crc_ok(report, INPUT_BLUETOOTH_LEN) => &report[2..],
        (Transport::Bluetooth, 0x01) if report.len() >= 10 => return Some(parse_simple(report)),
        _ => return None,
    };
    let left_trigger = common[4] as f32 / 255.0;
    let right_trigger = common[5] as f32 / 255.0;
    let mut buttons = playstation_face(common[7])
        | playstation_shoulders(common[8])
        | trigger_bits(left_trigger, right_trigger);
    for (mask, button) in [
        (0x01, GamepadButton::Home),
        (0x02, GamepadButton::Touchpad),
        (0x04, GamepadButton::Misc),
    ] {
        if common[9] & mask != 0 {
            buttons |= button.bit();
        }
    }
    let words = |at| [le16(common, at), le16(common, at + 2), le16(common, at + 4)];
    Some(GamepadReading {
        buttons,
        left_stick: stick(common[0], common[1]),
        right_stick: stick(common[2], common[3]),
        left_trigger,
        right_trigger,
        kind: GamepadKind::DualSense,
        motion: Some(cal.apply(words(15), words(21))),
    })
}

/// The reduced Bluetooth report: sticks, buttons and triggers only.
fn parse_simple(report: &[u8]) -> GamepadReading {
    let left_trigger = report[8] as f32 / 255.0;
    let right_trigger = report[9] as f32 / 255.0;
    let mut buttons = playstation_face(report[5])
        | playstation_shoulders(report[6])
        | trigger_bits(left_trigger, right_trigger);
    if report[7] & 0x01 != 0 {
        buttons |= GamepadButton::Home.bit();
    }
    if report[7] & 0x02 != 0 {
        buttons |= GamepadButton::Touchpad.bit();
    }
    GamepadReading {
        buttons,
        left_stick: stick(report[1], report[2]),
        right_stick: stick(report[3], report[4]),
        left_trigger,
        right_trigger,
        kind: GamepadKind::DualSense,
        motion: None,
    }
}

/// Reports put +y down; ours is up.
fn stick(x: u8, y: u8) -> [f32; 2] {
    radial_deadzone([axis_u8(x), -axis_u8(y)], STICK_DEADZONE)
}

/// Encodes `feedback` (rumble and trigger effects) as an output report into `out`, which must
/// hold at least [`OUTPUT_USB_LEN`] or [`OUTPUT_BLUETOOTH_LEN`] bytes. `sequence` (0..16) tags
/// Bluetooth reports; increment it per report. Returns the bytes written.
pub fn output(feedback: &Feedback, transport: Transport, sequence: u8, out: &mut [u8]) -> usize {
    let (len, common) = match transport {
        Transport::Usb => {
            out[..OUTPUT_USB_LEN].fill(0);
            out[0] = 0x02;
            (OUTPUT_USB_LEN, 1)
        }
        Transport::Bluetooth => {
            out[..OUTPUT_BLUETOOTH_LEN].fill(0);
            out[0] = 0x31;
            out[1] = (sequence & 0x0F) << 4;
            // Tag of a regular output report.
            out[2] = 0x10;
            (OUTPUT_BLUETOOTH_LEN, 3)
        }
    };
    let c = &mut out[common..];
    // Rumble through the compatible vibration path (rather than audio haptics), both the
    // original and the 2.24+ firmware flag; right and left trigger effects.
    c[0] = 0x01 | 0x02 | 0x04 | 0x08;
    c[38] = 0x04;
    c[2] = (feedback.rumble[1].clamp(0.0, 1.0) * 255.0) as u8;
    c[3] = (feedback.rumble[0].clamp(0.0, 1.0) * 255.0) as u8;
    c[10..21].copy_from_slice(&trigger_effect(&feedback.triggers[1]));
    c[21..32].copy_from_slice(&trigger_effect(&feedback.triggers[0]));
    if transport == Transport::Bluetooth {
        seal(out, len);
    }
    len
}

/// The 11-byte trigger effect block.
pub fn trigger_effect(effect: &TriggerEffect) -> [u8; 11] {
    let zone = |p: f32| (p.clamp(0.0, 1.0) * 9.0 + 0.5) as u32;
    let level = |s: f32| ((s.clamp(0.0, 1.0) * 8.0 + 0.5) as u32).clamp(1, 8) - 1;
    // Active zones from `start` to the end, each with a 3-bit level.
    let zones = |start: u32, value: u32| {
        let (mut active, mut levels) = (0u32, 0u32);
        for i in start..10 {
            active |= 1 << i;
            levels |= value << (3 * i);
        }
        (active, levels)
    };
    let mut b = [0u8; 11];
    match *effect {
        TriggerEffect::Off => b[0] = 0x05,
        TriggerEffect::Feedback { start, strength } => {
            if strength <= 0.0 {
                b[0] = 0x05;
            } else {
                let (active, levels) = zones(zone(start), level(strength));
                b[0] = 0x21;
                b[1..3].copy_from_slice(&(active as u16).to_le_bytes());
                b[3..7].copy_from_slice(&levels.to_le_bytes());
            }
        }
        TriggerEffect::Weapon {
            start,
            end,
            strength,
        } => {
            let start = zone(start).clamp(2, 7);
            let end = zone(end).clamp(start + 1, 8);
            let positions = (1u16 << start) | (1 << end);
            b[0] = 0x25;
            b[1..3].copy_from_slice(&positions.to_le_bytes());
            b[3] = level(strength) as u8;
        }
        TriggerEffect::Vibration {
            start,
            amplitude,
            frequency,
        } => {
            if amplitude <= 0.0 || frequency == 0 {
                b[0] = 0x05;
            } else {
                let (active, levels) = zones(zone(start), level(amplitude));
                b[0] = 0x26;
                b[1..3].copy_from_slice(&(active as u16).to_le_bytes());
                b[3..7].copy_from_slice(&levels.to_le_bytes());
                b[9] = frequency;
            }
        }
        TriggerEffect::Raw(raw) => b = raw,
    }
    b
}
