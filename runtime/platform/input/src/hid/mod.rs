//! Report formats of the controllers that need a driver of their own on PC: DualSense,
//! DualShock 4 and Switch Pro.
//!
//! Platform-neutral byte parsing and encoding. The Windows backend feeds it from `hid.dll`; the
//! Linux and macOS backends will feed it from hidraw and `IOHIDDevice`. Public so recorded
//! reports can be replayed and tools can talk to controllers directly.
//!
//! Reports start with their report id, as HID APIs deliver and expect them.

pub mod dualsense;
pub mod dualshock4;
pub mod switch_pro;

use crate::{GamepadButton, GamepadKind, HapticTone, TriggerEffect};

pub const VENDOR_SONY: u16 = 0x054C;
pub const VENDOR_NINTENDO: u16 = 0x057E;

/// The controller family of a USB or Bluetooth vendor/product id pair, if it has a driver here.
pub fn identify(vendor: u16, product: u16) -> Option<GamepadKind> {
    match (vendor, product) {
        (VENDOR_SONY, 0x0CE6 | 0x0DF2) => Some(GamepadKind::DualSense),
        (VENDOR_SONY, 0x05C4 | 0x09CC | 0x0BA0) => Some(GamepadKind::DualShock4),
        (VENDOR_NINTENDO, 0x2009) => Some(GamepadKind::SwitchPro),
        _ => None,
    }
}

/// How a controller is connected; the report layouts differ.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Transport {
    Usb,
    Bluetooth,
}

/// Everything a controller is asked to do; drivers encode the parts their hardware has.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Feedback {
    /// Motor speeds in [0, 1]: heavy (low frequency) and light (high frequency).
    pub rumble: [f32; 2],
    /// Adaptive trigger effects, left then right.
    pub triggers: [TriggerEffect; 2],
    /// HD rumble tones, left then right actuator; replaces `rumble` where supported.
    pub haptics: Option<[HapticTone; 2]>,
    /// Player number for the controller's indicator lights (1-based; 0 leaves them alone).
    pub player: u8,
}

/// The four face buttons and the hat of a PlayStation report byte.
fn playstation_face(byte: u8) -> u32 {
    let mut buttons = 0;
    for (mask, button) in [
        (0x10, GamepadButton::West),
        (0x20, GamepadButton::South),
        (0x40, GamepadButton::East),
        (0x80, GamepadButton::North),
    ] {
        if byte & mask != 0 {
            buttons |= button.bit();
        }
    }
    buttons | hat(byte & 0x0F)
}

/// A hat switch value (0 = up, clockwise in eighths; anything else = centered) as d-pad bits.
pub(crate) fn hat(value: u8) -> u32 {
    let (up, right, down, left) = (
        GamepadButton::DPadUp.bit(),
        GamepadButton::DPadRight.bit(),
        GamepadButton::DPadDown.bit(),
        GamepadButton::DPadLeft.bit(),
    );
    match value {
        0 => up,
        1 => up | right,
        2 => right,
        3 => right | down,
        4 => down,
        5 => down | left,
        6 => left,
        7 => left | up,
        _ => 0,
    }
}

/// Shoulders, Select/Start and stick clicks of a PlayStation report byte (the digital trigger
/// bits are ignored; triggers are derived from the analog values).
fn playstation_shoulders(byte: u8) -> u32 {
    let mut buttons = 0;
    for (mask, button) in [
        (0x01, GamepadButton::LeftShoulder),
        (0x02, GamepadButton::RightShoulder),
        (0x10, GamepadButton::Select),
        (0x20, GamepadButton::Start),
        (0x40, GamepadButton::LeftStick),
        (0x80, GamepadButton::RightStick),
    ] {
        if byte & mask != 0 {
            buttons |= button.bit();
        }
    }
    buttons
}

/// An 8-bit stick axis (128 = center) in [-1, 1].
fn axis_u8(value: u8) -> f32 {
    ((value as f32 - 128.0) / 127.0).clamp(-1.0, 1.0)
}

/// Dead zone of the HID sticks, as a fraction of full deflection. Their sticks are tighter than
/// XInput's recommended values assume.
pub(crate) const STICK_DEADZONE: f32 = 0.1;

/// Analog triggers set the digital trigger buttons past half way, as XInput does.
fn trigger_bits(left: f32, right: f32) -> u32 {
    let mut buttons = 0;
    if left > 0.5 {
        buttons |= GamepadButton::LeftTrigger.bit();
    }
    if right > 0.5 {
        buttons |= GamepadButton::RightTrigger.bit();
    }
    buttons
}

fn le16(bytes: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([bytes[at], bytes[at + 1]])
}

pub(crate) const GRAVITY: f32 = 9.806_65;
pub(crate) const RADIANS_PER_DEGREE: f32 = core::f32::consts::PI / 180.0;

/// Gyro and accelerometer calibration of a PlayStation controller: per axis, a bias in raw units
/// and a scale to rad/s or m/s².
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Calibration {
    pub gyro: [(f32, f32); 3],
    pub accel: [(f32, f32); 3],
}

impl Calibration {
    /// The sensors' nominal ranges (±2000 °/s, 8192 counts per g), for controllers whose
    /// calibration report could not be read.
    pub const NOMINAL: Self = Self {
        gyro: [(0.0, 2000.0 / 32767.0 * RADIANS_PER_DEGREE); 3],
        accel: [(0.0, GRAVITY / 8192.0); 3],
    };

    /// From the raw calibration words, in the order the DualSense report (and the DualShock 4
    /// Bluetooth report) stores them: three biases, plus/minus per gyro axis, the reference speed
    /// plus/minus, then plus/minus per accelerometer axis. Degenerate data gives `NOMINAL`.
    fn from_words(
        bias: [i16; 3],
        gyro: [(i16, i16); 3],
        speed: (i16, i16),
        accel: [(i16, i16); 3],
    ) -> Self {
        let mut cal = Self::NOMINAL;
        let speed_2x = speed.0 as f32 + speed.1 as f32;
        for i in 0..3 {
            let b = bias[i] as f32;
            let denominator = (gyro[i].0 as f32 - b).abs() + (gyro[i].1 as f32 - b).abs();
            if denominator > 0.0 && speed_2x > 0.0 {
                cal.gyro[i] = (b, speed_2x / denominator * RADIANS_PER_DEGREE);
            }
            let range_2g = accel[i].0 as f32 - accel[i].1 as f32;
            if range_2g > 0.0 {
                cal.accel[i] = (accel[i].0 as f32 - range_2g / 2.0, 2.0 * GRAVITY / range_2g);
            }
        }
        cal
    }

    /// Applies the calibration to raw sensor words (gyro then accel, x y z each).
    fn apply(&self, gyro: [i16; 3], accel: [i16; 3]) -> crate::Motion {
        let mut motion = crate::Motion::default();
        for i in 0..3 {
            let (bias, scale) = self.gyro[i];
            motion.gyro[i] = (gyro[i] as f32 - bias) * scale;
            let (bias, scale) = self.accel[i];
            motion.accel[i] = (accel[i] as f32 - bias) * scale;
        }
        motion
    }
}

impl Default for Calibration {
    fn default() -> Self {
        Self::NOMINAL
    }
}

/// CRC-32 (IEEE) over `seed` followed by `data`, as PlayStation controllers check Bluetooth
/// reports: seed 0xA1 for input, 0xA2 for output, 0xA3 for feature reports.
pub fn crc32(seed: u8, data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in core::iter::once(&seed).chain(data) {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// Whether a Bluetooth report of `len` bytes ends with a valid CRC-32 over the rest.
fn crc_ok(report: &[u8], len: usize) -> bool {
    report.len() >= len && {
        let expected = u32::from_le_bytes(report[len - 4..len].try_into().expect("4 bytes"));
        crc32(0xA1, &report[..len - 4]) == expected
    }
}

/// Appends the output CRC to a Bluetooth report of `len` bytes.
fn seal(out: &mut [u8], len: usize) {
    let crc = crc32(0xA2, &out[..len - 4]);
    out[len - 4..len].copy_from_slice(&crc.to_le_bytes());
}
