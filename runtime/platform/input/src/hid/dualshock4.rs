//! DualShock 4 (both revisions and the USB wireless adapter).
//!
//! Input: report 0x01 over USB (64 bytes). Over Bluetooth the controller starts with a reduced
//! 0x01 report and switches to the full 0x11 report (78 bytes, CRC-checked) once a calibration
//! feature report has been read. Output: report 0x05 over USB, 0x11 with a CRC over Bluetooth.

use super::{
    Calibration, Feedback, STICK_DEADZONE, Transport, axis_u8, crc_ok, le16, playstation_face,
    playstation_shoulders, seal, trigger_bits,
};
use crate::{GamepadButton, GamepadKind, GamepadReading, radial_deadzone};

/// The calibration feature report per transport; reading it over Bluetooth also enables the
/// full input report.
pub const CALIBRATION_REPORT_USB: u8 = 0x02;
pub const CALIBRATION_REPORT_BLUETOOTH: u8 = 0x05;
pub const CALIBRATION_LEN_USB: usize = 37;
pub const CALIBRATION_LEN_BLUETOOTH: usize = 41;
pub const OUTPUT_USB_LEN: usize = 32;
pub const OUTPUT_BLUETOOTH_LEN: usize = 78;

const INPUT_BLUETOOTH_LEN: usize = 78;

/// Parses a calibration feature report (id included). USB orders the gyro words all-plus then
/// all-minus; Bluetooth interleaves them.
pub fn calibration(report: &[u8], transport: Transport) -> Calibration {
    let (id, len) = match transport {
        Transport::Usb => (CALIBRATION_REPORT_USB, CALIBRATION_LEN_USB),
        Transport::Bluetooth => (CALIBRATION_REPORT_BLUETOOTH, CALIBRATION_LEN_BLUETOOTH),
    };
    if report.len() < len || report[0] != id {
        return Calibration::NOMINAL;
    }
    let w = |at| le16(report, at);
    let gyro = match transport {
        Transport::Usb => [(w(7), w(13)), (w(9), w(15)), (w(11), w(17))],
        Transport::Bluetooth => [(w(7), w(9)), (w(11), w(13)), (w(15), w(17))],
    };
    Calibration::from_words(
        [w(1), w(3), w(5)],
        gyro,
        (w(19), w(21)),
        [(w(23), w(25)), (w(27), w(29)), (w(31), w(33))],
    )
}

/// Parses an input report. `None` for other reports and Bluetooth reports with a bad CRC.
pub fn parse(report: &[u8], transport: Transport, cal: &Calibration) -> Option<GamepadReading> {
    let (common, sensors) = match (transport, report.first()?) {
        (Transport::Usb, 0x01) if report.len() >= 64 => (&report[1..], true),
        (Transport::Bluetooth, 0x11) if crc_ok(report, INPUT_BLUETOOTH_LEN) => (&report[3..], true),
        // The reduced report has the same layout up to the triggers.
        (Transport::Bluetooth, 0x01) if report.len() >= 10 => (&report[1..], false),
        _ => return None,
    };
    let left_trigger = common[7] as f32 / 255.0;
    let right_trigger = common[8] as f32 / 255.0;
    let mut buttons = playstation_face(common[4])
        | playstation_shoulders(common[5])
        | trigger_bits(left_trigger, right_trigger);
    if common[6] & 0x01 != 0 {
        buttons |= GamepadButton::Home.bit();
    }
    if common[6] & 0x02 != 0 {
        buttons |= GamepadButton::Touchpad.bit();
    }
    let words = |at| [le16(common, at), le16(common, at + 2), le16(common, at + 4)];
    Some(GamepadReading {
        buttons,
        left_stick: stick(common[0], common[1]),
        right_stick: stick(common[2], common[3]),
        left_trigger,
        right_trigger,
        kind: GamepadKind::DualShock4,
        motion: sensors.then(|| cal.apply(words(12), words(18))),
    })
}

fn stick(x: u8, y: u8) -> [f32; 2] {
    radial_deadzone([axis_u8(x), -axis_u8(y)], STICK_DEADZONE)
}

/// Encodes the rumble of `feedback` as an output report into `out`, which must hold at least
/// [`OUTPUT_USB_LEN`] or [`OUTPUT_BLUETOOTH_LEN`] bytes. Leaves the light bar alone. Returns the
/// bytes written.
pub fn output(feedback: &Feedback, transport: Transport, out: &mut [u8]) -> usize {
    let (len, common) = match transport {
        Transport::Usb => {
            out[..OUTPUT_USB_LEN].fill(0);
            out[0] = 0x05;
            (OUTPUT_USB_LEN, 1)
        }
        Transport::Bluetooth => {
            out[..OUTPUT_BLUETOOTH_LEN].fill(0);
            out[0] = 0x11;
            // HID report with CRC; sensor samples every 4 ms.
            out[1] = 0xC4;
            (OUTPUT_BLUETOOTH_LEN, 3)
        }
    };
    let c = &mut out[common..];
    // Only the motors are valid in this report.
    c[0] = 0x01;
    c[3] = (feedback.rumble[1].clamp(0.0, 1.0) * 255.0) as u8;
    c[4] = (feedback.rumble[0].clamp(0.0, 1.0) * 255.0) as u8;
    if transport == Transport::Bluetooth {
        seal(out, len);
    }
    len
}
