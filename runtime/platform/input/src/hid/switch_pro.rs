//! Nintendo Switch Pro Controller.
//!
//! The controller needs commands before it reports usefully: over USB a handshake that keeps it
//! talking HID, then (both transports) the full 0x30 input mode with the IMU and vibration
//! enabled. [`command`] yields them one at a time; send one per poll. Input 0x30 (and the 0x21
//! command replies) carry buttons and 12-bit sticks; 0x30 adds three IMU samples. Rumble is the
//! 0x10 output report, two HD rumble actuators of two tones each.
//!
//! Stick and IMU scales are nominal: the factory calibration in the controller's SPI flash is
//! not read yet, so sticks may stop short of full deflection.

use super::{Feedback, GRAVITY, RADIANS_PER_DEGREE, Transport, le16};
use crate::{GamepadButton, GamepadKind, GamepadReading, HapticTone, Motion, radial_deadzone};

/// Bytes [`command`] and [`rumble`] write at most. Pad to the device's output length.
pub const OUTPUT_LEN: usize = 49;

/// Nominal stick center and half-range in raw 12-bit units.
const STICK_CENTER: f32 = 2048.0;
const STICK_RANGE: f32 = 1450.0;
const STICK_DEADZONE: f32 = 0.12;
/// LSM6DS3 at ±8 g and ±2000 °/s.
const ACCEL_SCALE: f32 = 0.000_244 * GRAVITY;
const GYRO_SCALE: f32 = 0.070 * RADIANS_PER_DEGREE;

/// The neutral HD rumble frame of one actuator: 320 Hz and 160 Hz at amplitude 0.
const NEUTRAL: [u8; 4] = [0x00, 0x01, 0x40, 0x40];

/// The `step`th setup command, written into `out` (at least [`OUTPUT_LEN`] bytes): `None` once
/// setup is complete. `counter` is the 4-bit packet counter (increment per output report);
/// `player` (1–4) sets the indicator lights, 0 skips that step.
pub fn command(
    step: usize,
    transport: Transport,
    counter: u8,
    player: u8,
    out: &mut [u8],
) -> Option<usize> {
    out[..OUTPUT_LEN].fill(0);
    // USB: handshake, 3 Mbit baud rate, handshake again at that rate, then "HID only, no
    // timeout" so the controller does not fall back to its proprietary USB protocol.
    const USB: [u8; 4] = [0x02, 0x03, 0x02, 0x04];
    let step = match transport {
        Transport::Usb if step < USB.len() => {
            out[0] = 0x80;
            out[1] = USB[step];
            return Some(2);
        }
        Transport::Usb => step - USB.len(),
        Transport::Bluetooth => step,
    };
    let lights = match player {
        1..=4 => (1u8 << player) - 1,
        _ => 0,
    };
    // Subcommands: enable the IMU, enable vibration, full input report mode, player lights.
    let (id, arg) = match step {
        0 => (0x40, 0x01),
        1 => (0x48, 0x01),
        2 => (0x03, 0x30),
        3 if player != 0 => (0x30, lights),
        _ => return None,
    };
    out[0] = 0x01;
    out[1] = counter & 0x0F;
    out[2..6].copy_from_slice(&NEUTRAL);
    out[6..10].copy_from_slice(&NEUTRAL);
    out[10] = id;
    out[11] = arg;
    Some(12)
}

/// Parses a 0x30 input report or the input part of a 0x21 command reply.
pub fn parse(report: &[u8]) -> Option<GamepadReading> {
    let imu = match report.first()? {
        0x30 if report.len() >= 49 => true,
        0x21 if report.len() >= 13 => false,
        _ => return None,
    };
    let (right, shared, left) = (report[3], report[4], report[5]);
    let mut buttons = 0;
    for (byte, mask, button) in [
        // Positional names: Nintendo's A is on the east side.
        (right, 0x01, GamepadButton::West),
        (right, 0x02, GamepadButton::North),
        (right, 0x04, GamepadButton::South),
        (right, 0x08, GamepadButton::East),
        (right, 0x40, GamepadButton::RightShoulder),
        (right, 0x80, GamepadButton::RightTrigger),
        (shared, 0x01, GamepadButton::Select),
        (shared, 0x02, GamepadButton::Start),
        (shared, 0x04, GamepadButton::RightStick),
        (shared, 0x08, GamepadButton::LeftStick),
        (shared, 0x10, GamepadButton::Home),
        (shared, 0x20, GamepadButton::Misc),
        (left, 0x01, GamepadButton::DPadDown),
        (left, 0x02, GamepadButton::DPadUp),
        (left, 0x04, GamepadButton::DPadRight),
        (left, 0x08, GamepadButton::DPadLeft),
        (left, 0x40, GamepadButton::LeftShoulder),
        (left, 0x80, GamepadButton::LeftTrigger),
    ] {
        if byte & mask != 0 {
            buttons |= button.bit();
        }
    }
    // ZL and ZR are digital.
    let trigger = |b: GamepadButton| if buttons & b.bit() != 0 { 1.0 } else { 0.0 };
    let motion = imu.then(|| {
        // Three samples 5 ms apart; their mean is plenty for a frame.
        let mut sum = [0i32; 6];
        for sample in 0..3 {
            for (axis, s) in sum.iter_mut().enumerate() {
                *s += le16(report, 13 + sample * 12 + axis * 2) as i32;
            }
        }
        let mean = sum.map(|s| s as f32 / 3.0);
        // The IMU's axes, shuffled to the PlayStation frame (+x right, +y up, +z toward the
        // player).
        let (ax, ay, az, gx, gy, gz) = (mean[0], mean[1], mean[2], mean[3], mean[4], mean[5]);
        Motion {
            gyro: [-gy * GYRO_SCALE, gz * GYRO_SCALE, -gx * GYRO_SCALE],
            accel: [-ay * ACCEL_SCALE, az * ACCEL_SCALE, -ax * ACCEL_SCALE],
        }
    });
    Some(GamepadReading {
        buttons,
        left_stick: stick(&report[6..9]),
        right_stick: stick(&report[9..12]),
        left_trigger: trigger(GamepadButton::LeftTrigger),
        right_trigger: trigger(GamepadButton::RightTrigger),
        kind: GamepadKind::SwitchPro,
        motion,
    })
}

/// Two 12-bit values packed in three bytes; +y is already up.
fn stick(b: &[u8]) -> [f32; 2] {
    let x = b[0] as u16 | ((b[1] as u16 & 0x0F) << 8);
    let y = (b[1] as u16 >> 4) | ((b[2] as u16) << 4);
    let axis = |v: u16| ((v as f32 - STICK_CENTER) / STICK_RANGE).clamp(-1.0, 1.0);
    radial_deadzone([axis(x), axis(y)], STICK_DEADZONE)
}

/// Encodes the rumble (or HD rumble tones) of `feedback` as a 0x10 output report into `out` (at
/// least [`OUTPUT_LEN`] bytes). Plain rumble plays the heavy motor as a 160 Hz tone and the
/// light one as 320 Hz on both actuators. Returns the bytes written.
pub fn rumble(feedback: &Feedback, counter: u8, out: &mut [u8]) -> usize {
    let [left, right] = feedback.haptics.unwrap_or_else(|| {
        let tone = HapticTone {
            low_frequency: 160.0,
            low_amplitude: feedback.rumble[0],
            high_frequency: 320.0,
            high_amplitude: feedback.rumble[1],
        };
        [tone, tone]
    });
    out[..OUTPUT_LEN].fill(0);
    out[0] = 0x10;
    out[1] = counter & 0x0F;
    out[2..6].copy_from_slice(&encode_tone(&left));
    out[6..10].copy_from_slice(&encode_tone(&right));
    10
}

/// One actuator's HD rumble frame: high-band frequency and amplitude, low-band frequency and
/// amplitude, interleaved as the controller expects.
pub fn encode_tone(tone: &HapticTone) -> [u8; 4] {
    // Frequencies are steps of 2^(1/32) above 10 Hz; each band covers 128 of them.
    let step = |hz: f32| (log2(hz.max(10.0) / 10.0) * 32.0 + 0.5) as i32;
    let high = ((step(tone.high_frequency) - 0x60).clamp(0x01, 0x7F) as u16) << 2;
    let low = (step(tone.low_frequency) - 0x40).clamp(0x01, 0x7F) as u8;
    let high_amplitude = amplitude(tone.high_amplitude) << 1;
    let low_amplitude = amplitude(tone.low_amplitude);
    [
        high as u8,
        high_amplitude + (high >> 8) as u8,
        low | (low_amplitude & 1) << 7,
        (low_amplitude >> 1) + 0x40,
    ]
}

/// The controller's logarithmic amplitude code, 0 to 100.
fn amplitude(amp: f32) -> u8 {
    let amp = amp.clamp(0.0, 1.0);
    let code = if amp > 0.23 {
        log2(amp * 8.7) * 32.0
    } else if amp > 1.0 / 17.0 {
        log2(amp * 17.0) * 16.0
    } else {
        0.0
    };
    ((code + 0.5) as i32).clamp(0, 100) as u8
}

/// log2 for positive normal `x`: exponent plus a series for the mantissa (error < 1e-6).
fn log2(x: f32) -> f32 {
    let bits = x.to_bits();
    let exponent = ((bits >> 23) & 0xFF) as i32 - 127;
    let m = f32::from_bits((bits & 0x007F_FFFF) | 0x3F80_0000);
    // ln(m) = 2 atanh(t), t = (m - 1) / (m + 1) in [0, 1/3).
    let t = (m - 1.0) / (m + 1.0);
    let t2 = t * t;
    let series = t * (1.0 + t2 * (1.0 / 3.0 + t2 * (1.0 / 5.0 + t2 * (1.0 / 7.0 + t2 / 9.0))));
    exponent as f32 + 2.0 * series * core::f32::consts::LOG2_E
}
