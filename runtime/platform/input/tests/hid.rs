//! HID report parsing and encoding, from hand-built reports laid out as the controllers send
//! them. Platform-independent.

use marlin_input::hid::{
    self, Calibration, Feedback, Transport, dualsense, dualshock4, switch_pro,
};
use marlin_input::{GamepadButton as B, GamepadKind, HapticTone, TriggerEffect};

const RAD: f32 = std::f32::consts::PI / 180.0;

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-4 * b.abs().max(1.0)
}

fn bits(buttons: &[B]) -> u32 {
    buttons.iter().fold(0, |acc, b| acc | b.bit())
}

/// Seals a Bluetooth input report of `len` bytes with its CRC.
fn seal_input(report: &mut [u8], len: usize) {
    let crc = hid::crc32(0xA1, &report[..len - 4]);
    report[len - 4..len].copy_from_slice(&crc.to_le_bytes());
}

fn output_crc_ok(out: &[u8], len: usize) -> bool {
    hid::crc32(0xA2, &out[..len - 4]).to_le_bytes() == out[len - 4..len]
}

fn put16(buf: &mut [u8], at: usize, v: i16) {
    buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

#[test]
fn identifies_controllers() {
    assert_eq!(hid::identify(0x054C, 0x0CE6), Some(GamepadKind::DualSense));
    assert_eq!(hid::identify(0x054C, 0x0DF2), Some(GamepadKind::DualSense));
    assert_eq!(hid::identify(0x054C, 0x09CC), Some(GamepadKind::DualShock4));
    assert_eq!(hid::identify(0x057E, 0x2009), Some(GamepadKind::SwitchPro));
    assert_eq!(hid::identify(0x045E, 0x02FF), None);
}

#[test]
fn crc32_matches_the_standard_check_value() {
    // CRC-32/ISO-HDLC of "123456789"; the seed is just the first byte.
    assert_eq!(hid::crc32(b'1', b"23456789"), 0xCBF4_3926);
}

/// The common part of a DualSense input report (63 bytes after the id).
fn dualsense_common() -> [u8; 63] {
    let mut c = [0u8; 63];
    c[..4].copy_from_slice(&[255, 128, 128, 0]);
    c[4] = 255; // L2
    c[5] = 64; // R2, below half
    c[7] = 0x20 | 0x02; // Cross, hat east
    c[8] = 0x01 | 0x20; // L1, Options
    c[9] = 0x01 | 0x04; // PS, mute
    put16(&mut c, 15, 1000); // gyro pitch
    put16(&mut c, 17, -500); // gyro yaw
    put16(&mut c, 23, 8192); // accel y
    c
}

#[test]
fn dualsense_usb_report() {
    let mut report = [0u8; 64];
    report[0] = 0x01;
    report[1..].copy_from_slice(&dualsense_common());
    let r = dualsense::parse(&report, Transport::Usb, &Calibration::NOMINAL).unwrap();
    assert_eq!(r.kind, GamepadKind::DualSense);
    assert_eq!(
        r.buttons,
        bits(&[
            B::South,
            B::DPadRight,
            B::LeftShoulder,
            B::Start,
            B::Home,
            B::Misc,
            B::LeftTrigger,
        ])
    );
    assert_eq!(r.left_stick, [1.0, 0.0]);
    assert_eq!(r.right_stick, [0.0, 1.0], "report +y is down");
    assert_eq!((r.left_trigger, r.right_trigger), (1.0, 64.0 / 255.0));
    let motion = r.motion.unwrap();
    assert!(close(motion.gyro[0], 1000.0 * 2000.0 / 32767.0 * RAD));
    assert!(close(motion.gyro[1], -500.0 * 2000.0 / 32767.0 * RAD));
    assert!(close(motion.accel[1], 9.80665));

    // Wrong id or short report.
    report[0] = 0x02;
    assert!(dualsense::parse(&report, Transport::Usb, &Calibration::NOMINAL).is_none());
    assert!(dualsense::parse(&report[..20], Transport::Usb, &Calibration::NOMINAL).is_none());
}

#[test]
fn dualsense_bluetooth_reports() {
    let mut report = [0u8; 78];
    report[0] = 0x31;
    report[2..65].copy_from_slice(&dualsense_common());
    seal_input(&mut report, 78);
    let full = dualsense::parse(&report, Transport::Bluetooth, &Calibration::NOMINAL).unwrap();
    assert!(full.motion.is_some());
    assert_eq!(full.left_stick, [1.0, 0.0]);

    report[10] ^= 1;
    assert!(
        dualsense::parse(&report, Transport::Bluetooth, &Calibration::NOMINAL).is_none(),
        "bad CRC"
    );

    // The reduced report before the calibration read: no sensors.
    let mut simple = [0u8; 78];
    simple[..10].copy_from_slice(&[0x01, 128, 128, 255, 128, 0x80 | 0x08, 0x02, 0x02, 0, 200]);
    let r = dualsense::parse(&simple, Transport::Bluetooth, &Calibration::NOMINAL).unwrap();
    assert_eq!(
        r.buttons,
        bits(&[B::North, B::RightShoulder, B::Touchpad, B::RightTrigger])
    );
    assert_eq!(r.right_stick, [1.0, 0.0]);
    assert!(r.motion.is_none());
}

#[test]
fn dualsense_calibration() {
    let mut feature = [0u8; dualsense::CALIBRATION_LEN];
    feature[0] = dualsense::CALIBRATION_REPORT;
    for (i, v) in [8000, -8000, 8000, -8000, 8000, -8000].iter().enumerate() {
        put16(&mut feature, 7 + 2 * i, *v);
    }
    put16(&mut feature, 19, 540);
    put16(&mut feature, 21, 540);
    for (i, v) in [8192, -8192, 8192, -8192, 8192, -8192].iter().enumerate() {
        put16(&mut feature, 23 + 2 * i, *v);
    }
    let cal = dualsense::calibration(&feature);
    // 1080 °/s over 16000 counts; 2 g over 16384 counts.
    for i in 0..3 {
        assert_eq!(cal.gyro[i].0, 0.0);
        assert!(close(cal.gyro[i].1, 1080.0 / 16000.0 * RAD));
        assert!(close(cal.accel[i].1, 9.80665 / 8192.0));
    }

    let mut report = [0u8; 64];
    report[0] = 0x01;
    report[1..].copy_from_slice(&dualsense_common());
    put16(&mut report, 16, 16000);
    let motion = dualsense::parse(&report, Transport::Usb, &cal)
        .unwrap()
        .motion
        .unwrap();
    assert!(close(motion.gyro[0], 1080.0 * RAD));

    // Degenerate or foreign data falls back to the nominal scales.
    assert_eq!(dualsense::calibration(&[0x05; 10]), Calibration::NOMINAL);
    assert_eq!(dualsense::calibration(&[0u8; 41]), Calibration::NOMINAL);
}

#[test]
fn dualsense_output() {
    let feedback = Feedback {
        rumble: [1.0, 0.5],
        triggers: [
            TriggerEffect::Weapon {
                start: 0.2,
                end: 0.6,
                strength: 1.0,
            },
            TriggerEffect::Off,
        ],
        ..Feedback::default()
    };
    let mut out = [0xAAu8; 80];
    assert_eq!(
        dualsense::output(&feedback, Transport::Usb, 0, &mut out),
        dualsense::OUTPUT_USB_LEN
    );
    assert_eq!(out[0], 0x02);
    assert_eq!(out[1], 0x0F, "rumble and both trigger effects valid");
    assert_eq!((out[3], out[4]), (127, 255), "light motor, heavy motor");
    assert_eq!(out[11], 0x05, "right trigger off");
    assert_eq!(out[22..26], [0x25, 0x24, 0x00, 7], "left trigger weapon");
    assert_eq!(out[39], 0x04);
    assert_eq!(out[48], 0xAA, "nothing written past the report");

    let n = dualsense::output(&feedback, Transport::Bluetooth, 5, &mut out);
    assert_eq!(n, dualsense::OUTPUT_BLUETOOTH_LEN);
    assert_eq!(out[..4], [0x31, 0x50, 0x10, 0x0F]);
    assert_eq!((out[5], out[6]), (127, 255));
    assert!(output_crc_ok(&out, n));
}

#[test]
fn trigger_effects() {
    let effect = dualsense::trigger_effect;
    assert_eq!(
        effect(&TriggerEffect::Off),
        [0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(
        effect(&TriggerEffect::Feedback {
            start: 0.0,
            strength: 1.0
        }),
        [0x21, 0xFF, 0x03, 0xFF, 0xFF, 0xFF, 0x3F, 0, 0, 0, 0]
    );
    // Zones 5–9 at level 4 (stored as 3).
    assert_eq!(
        effect(&TriggerEffect::Feedback {
            start: 0.5,
            strength: 0.5
        }),
        [0x21, 0xE0, 0x03, 0x00, 0x80, 0x6D, 0x1B, 0, 0, 0, 0]
    );
    assert_eq!(
        effect(&TriggerEffect::Feedback {
            start: 0.5,
            strength: 0.0
        })[0],
        0x05,
        "zero strength is off"
    );
    assert_eq!(
        effect(&TriggerEffect::Vibration {
            start: 0.0,
            amplitude: 1.0,
            frequency: 30
        }),
        [0x26, 0xFF, 0x03, 0xFF, 0xFF, 0xFF, 0x3F, 0, 0, 30, 0]
    );
    // Weapon positions are kept in the range the controller accepts.
    assert_eq!(
        effect(&TriggerEffect::Weapon {
            start: 0.0,
            end: 0.0,
            strength: 0.5
        })[..4],
        [0x25, 0x0C, 0x00, 3]
    );
    let raw = [0x26, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    assert_eq!(effect(&TriggerEffect::Raw(raw)), raw);
}

fn dualshock4_common() -> [u8; 31] {
    let mut c = [0u8; 31];
    c[..4].copy_from_slice(&[128, 128, 128, 128]);
    c[4] = 0x80 | 0x08; // Triangle, hat centered
    c[5] = 0x02 | 0x10; // R1, Share
    c[6] = 0x02; // touchpad
    c[8] = 255; // R2
    put16(&mut c, 14, 100); // gyro yaw
    put16(&mut c, 22, -8192); // accel z
    c
}

#[test]
fn dualshock4_reports() {
    let expected = bits(&[
        B::North,
        B::RightShoulder,
        B::Select,
        B::Touchpad,
        B::RightTrigger,
    ]);
    let mut usb = [0u8; 64];
    usb[0] = 0x01;
    usb[1..32].copy_from_slice(&dualshock4_common());
    let r = dualshock4::parse(&usb, Transport::Usb, &Calibration::NOMINAL).unwrap();
    assert_eq!(r.kind, GamepadKind::DualShock4);
    assert_eq!(r.buttons, expected);
    assert_eq!(r.left_stick, [0.0, 0.0]);
    let motion = r.motion.unwrap();
    assert!(close(motion.gyro[1], 100.0 * 2000.0 / 32767.0 * RAD));
    assert!(close(motion.accel[2], -9.80665));

    let mut bt = [0u8; 78];
    bt[0] = 0x11;
    bt[1] = 0xC0;
    bt[3..34].copy_from_slice(&dualshock4_common());
    seal_input(&mut bt, 78);
    let r = dualshock4::parse(&bt, Transport::Bluetooth, &Calibration::NOMINAL).unwrap();
    assert_eq!(r.buttons, expected);
    assert!(r.motion.is_some());
    bt[40] ^= 0x80;
    assert!(dualshock4::parse(&bt, Transport::Bluetooth, &Calibration::NOMINAL).is_none());

    // The reduced Bluetooth report shares the USB layout up to the triggers.
    let mut simple = [0u8; 78];
    simple[0] = 0x01;
    simple[1..10].copy_from_slice(&dualshock4_common()[..9]);
    let r = dualshock4::parse(&simple, Transport::Bluetooth, &Calibration::NOMINAL).unwrap();
    assert_eq!(r.buttons, expected);
    assert!(r.motion.is_none());
}

#[test]
fn dualshock4_calibration_orders_differ_by_transport() {
    let plus_minus = [(1000, -1000), (2000, -2000), (3000, -3000)];
    let mut usb = [0u8; dualshock4::CALIBRATION_LEN_USB];
    usb[0] = dualshock4::CALIBRATION_REPORT_USB;
    let mut bt = [0u8; dualshock4::CALIBRATION_LEN_BLUETOOTH];
    bt[0] = dualshock4::CALIBRATION_REPORT_BLUETOOTH;
    for (i, (plus, minus)) in plus_minus.iter().enumerate() {
        put16(&mut usb, 7 + 2 * i, *plus);
        put16(&mut usb, 13 + 2 * i, *minus);
        put16(&mut bt, 7 + 4 * i, *plus);
        put16(&mut bt, 9 + 4 * i, *minus);
    }
    for f in [&mut usb[..], &mut bt[..]] {
        put16(f, 19, 500);
        put16(f, 21, 500);
    }
    let usb = dualshock4::calibration(&usb, Transport::Usb);
    let bt = dualshock4::calibration(&bt, Transport::Bluetooth);
    assert_eq!(usb, bt);
    for (i, (plus, _)) in plus_minus.iter().enumerate() {
        assert!(close(usb.gyro[i].1, 1000.0 / (2.0 * *plus as f32) * RAD));
    }
    // A USB report read as Bluetooth is rejected by its id.
    assert_eq!(
        dualshock4::calibration(
            &[dualshock4::CALIBRATION_REPORT_USB; 41],
            Transport::Bluetooth
        ),
        Calibration::NOMINAL
    );
}

#[test]
fn dualshock4_output() {
    let feedback = Feedback {
        rumble: [0.0, 1.0],
        ..Feedback::default()
    };
    let mut out = [0u8; 80];
    assert_eq!(dualshock4::output(&feedback, Transport::Usb, &mut out), 32);
    assert_eq!(out[..6], [0x05, 0x01, 0, 0, 255, 0]);
    let n = dualshock4::output(&feedback, Transport::Bluetooth, &mut out);
    assert_eq!(out[..8], [0x11, 0xC4, 0, 0x01, 0, 0, 255, 0]);
    assert!(output_crc_ok(&out, n));
}

/// A Switch Pro 0x30 report with three identical IMU samples.
fn switch_report() -> [u8; 64] {
    let mut r = [0u8; 64];
    r[0] = 0x30;
    r[3] = 0x08 | 0x80; // A, ZR
    r[4] = 0x10 | 0x02; // Home, Plus
    r[5] = 0x02 | 0x40; // Up, L
    r[6..9].copy_from_slice(&[0x00, 0x08, 0x80]); // left stick centered (2048, 2048)
    r[9..12].copy_from_slice(&[0xFF, 0x0F, 0x80]); // right stick (4095, 2048)
    for sample in 0..3 {
        let at = 13 + sample * 12;
        put16(&mut r, at + 4, 4096); // accel z
        put16(&mut r, at + 6, 1000); // gyro x
    }
    r
}

#[test]
fn switch_pro_reports() {
    let r = switch_pro::parse(&switch_report()).unwrap();
    assert_eq!(r.kind, GamepadKind::SwitchPro);
    // Nintendo's A sits east.
    assert_eq!(
        r.buttons,
        bits(&[
            B::East,
            B::RightTrigger,
            B::Home,
            B::Start,
            B::DPadUp,
            B::LeftShoulder
        ])
    );
    assert_eq!((r.left_trigger, r.right_trigger), (0.0, 1.0));
    assert_eq!(r.left_stick, [0.0, 0.0]);
    assert_eq!(r.right_stick, [1.0, 0.0]);
    let motion = r.motion.unwrap();
    // IMU z is the PlayStation y; IMU x is minus z.
    assert!(close(motion.accel[1], 4096.0 * 0.000244 * 9.80665));
    assert!(close(motion.gyro[2], -1000.0 * 0.070 * RAD));
    assert_eq!((motion.gyro[0], motion.gyro[1]), (0.0, 0.0));

    // Command replies carry buttons and sticks but no IMU.
    let mut reply = switch_report();
    reply[0] = 0x21;
    let r = switch_pro::parse(&reply).unwrap();
    assert!(r.motion.is_none());
    assert!(r.buttons & B::East.bit() != 0);
    assert!(switch_pro::parse(&[0x3F; 12]).is_none());
}

#[test]
fn switch_pro_setup_commands() {
    let mut out = [0u8; 64];
    let mut usb = Vec::new();
    for step in 0.. {
        let Some(n) = switch_pro::command(step, Transport::Usb, step as u8, 2, &mut out) else {
            break;
        };
        usb.push(out[..n].to_vec());
    }
    assert_eq!(usb.len(), 8);
    assert_eq!(
        usb[..4],
        [[0x80, 0x02], [0x80, 0x03], [0x80, 0x02], [0x80, 0x04]]
    );
    // Subcommands: neutral rumble, then id and argument.
    let subcommands: Vec<_> = usb[4..].iter().map(|c| (c[0], c[10], c[11])).collect();
    assert_eq!(
        subcommands,
        [
            (0x01, 0x40, 0x01),
            (0x01, 0x48, 0x01),
            (0x01, 0x03, 0x30),
            (0x01, 0x30, 0b11)
        ]
    );
    assert_eq!(usb[4][1], 4, "packet counter");
    assert_eq!(
        usb[4][2..10],
        [0x00, 0x01, 0x40, 0x40, 0x00, 0x01, 0x40, 0x40]
    );

    // Bluetooth skips the USB handshake; player 0 skips the lights.
    let n = switch_pro::command(0, Transport::Bluetooth, 0, 0, &mut out).unwrap();
    assert_eq!((n, out[0], out[10]), (12, 0x01, 0x40));
    assert!(switch_pro::command(3, Transport::Bluetooth, 0, 0, &mut out).is_none());
}

#[test]
fn switch_pro_rumble_encoding() {
    assert_eq!(
        switch_pro::encode_tone(&HapticTone::OFF),
        [0x00, 0x01, 0x40, 0x40]
    );
    let full = HapticTone {
        low_frequency: 160.0,
        low_amplitude: 1.0,
        high_frequency: 320.0,
        high_amplitude: 1.0,
    };
    // Amplitude codes top out at 0xC8 (high band) and 0x72 (low band).
    assert_eq!(switch_pro::encode_tone(&full), [0x00, 0xC9, 0x40, 0x72]);
    // Frequencies are clamped to each band.
    let extreme = HapticTone {
        low_frequency: 1.0,
        high_frequency: 100_000.0,
        ..HapticTone::OFF
    };
    let [h0, h1, l, _] = switch_pro::encode_tone(&extreme);
    assert_eq!((h0, h1 & 1, l), (0xFC, 0x01, 0x01));

    let mut out = [0u8; 64];
    let feedback = Feedback {
        rumble: [1.0, 0.0],
        ..Feedback::default()
    };
    assert_eq!(switch_pro::rumble(&feedback, 3, &mut out), 10);
    assert_eq!(out[..2], [0x10, 3]);
    assert_eq!(
        out[2..6],
        out[6..10],
        "plain rumble drives both actuators alike"
    );
    assert_eq!(out[2..4], [0x00, 0x01], "light motor silent");
    assert_eq!(out[5], 0x72, "heavy motor at full amplitude");

    let left = HapticTone {
        high_amplitude: 0.5,
        ..HapticTone::OFF
    };
    let feedback = Feedback {
        haptics: Some([left, HapticTone::OFF]),
        ..feedback
    };
    switch_pro::rumble(&feedback, 0, &mut out);
    assert_ne!(out[2..6], out[6..10]);
    assert_eq!(out[6..10], [0x00, 0x01, 0x40, 0x40]);
}
