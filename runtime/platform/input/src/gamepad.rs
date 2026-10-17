/// Gamepad slots: enough for local multiplayer with Xbox, PlayStation, Switch and generic pads
/// connected together.
pub const MAX_GAMEPADS: usize = 8;

/// Gamepad buttons, named by position so the same binding means the same physical button on
/// every controller (South is A on Xbox, Cross on PlayStation, B on Nintendo).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum GamepadButton {
    South,
    East,
    West,
    North,
    LeftShoulder,
    RightShoulder,
    /// Digital view of the analog trigger (past half way).
    LeftTrigger,
    RightTrigger,
    /// View / Back / Share / Create / Minus.
    Select,
    /// Menu / Start / Options / Plus.
    Start,
    LeftStick,
    RightStick,
    DPadUp,
    DPadDown,
    DPadLeft,
    DPadRight,
    /// Guide / PS / Home. Xbox pads read through XInput never report it (the OS keeps it).
    Home,
    /// The PlayStation touchpad click.
    Touchpad,
    /// Capture (Switch), microphone mute (DualSense).
    Misc,
}

impl GamepadButton {
    #[inline]
    pub const fn bit(self) -> u32 {
        1 << self as u8
    }
}

/// The controller family, for button prompts and for knowing which extensions apply.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum GamepadKind {
    /// A HID gamepad without a driver of its own; buttons are mapped by a heuristic.
    #[default]
    Generic,
    Xbox,
    DualShock4,
    DualSense,
    SwitchPro,
}

/// Motion sensors of a gamepad, in the controller's frame: +x right, +y up, +z toward the player
/// (out of the face), the axes PlayStation controllers use.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Motion {
    /// Angular velocity in radians per second (right-handed about each axis).
    pub gyro: [f32; 3],
    /// Acceleration including gravity, in m/s²: at rest it points away from the floor with a
    /// length of about 9.8.
    pub accel: [f32; 3],
}

/// One poll of a gamepad, after dead zones. Backends produce these.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct GamepadReading {
    /// [`GamepadButton::bit`]s.
    pub buttons: u32,
    /// [-1, 1] per axis; positive is right and up.
    pub left_stick: [f32; 2],
    pub right_stick: [f32; 2],
    /// [0, 1].
    pub left_trigger: f32,
    pub right_trigger: f32,
    pub kind: GamepadKind,
    /// `None` without sensors (or with them switched off).
    pub motion: Option<Motion>,
}

/// Applies a radial dead zone to a stick and rescales the rest to the full [0, 1] range, so small
/// deflections stay smooth. `deadzone` is a fraction of full deflection.
pub fn radial_deadzone(stick: [f32; 2], deadzone: f32) -> [f32; 2] {
    let [x, y] = stick;
    let magnitude = core::f32::math::sqrt(x * x + y * y);
    if magnitude <= deadzone {
        return [0.0, 0.0];
    }
    let scaled = (magnitude.min(1.0) - deadzone) / (1.0 - deadzone);
    let k = scaled / magnitude;
    [x * k, y * k]
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GamepadState {
    pub(crate) connected: bool,
    pub(crate) reading: GamepadReading,
    pub(crate) pressed: u32,
    pub(crate) released: u32,
}

impl GamepadState {
    pub(crate) const DISCONNECTED: Self = Self {
        connected: false,
        reading: GamepadReading {
            buttons: 0,
            left_stick: [0.0; 2],
            right_stick: [0.0; 2],
            left_trigger: 0.0,
            right_trigger: 0.0,
            kind: GamepadKind::Generic,
            motion: None,
        },
        pressed: 0,
        released: 0,
    };

    #[inline]
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    #[inline]
    pub fn kind(&self) -> GamepadKind {
        self.reading.kind
    }

    #[inline]
    pub fn down(&self, button: GamepadButton) -> bool {
        self.reading.buttons & button.bit() != 0
    }

    #[inline]
    pub fn pressed(&self, button: GamepadButton) -> bool {
        self.pressed & button.bit() != 0
    }

    #[inline]
    pub fn released(&self, button: GamepadButton) -> bool {
        self.released & button.bit() != 0
    }

    #[inline]
    pub fn left_stick(&self) -> [f32; 2] {
        self.reading.left_stick
    }

    #[inline]
    pub fn right_stick(&self) -> [f32; 2] {
        self.reading.right_stick
    }

    #[inline]
    pub fn left_trigger(&self) -> f32 {
        self.reading.left_trigger
    }

    #[inline]
    pub fn right_trigger(&self) -> f32 {
        self.reading.right_trigger
    }

    /// Read through the `gyro` extension, which only platforms with motion support declare.
    #[cfg_attr(not(marlin_ext = "gyro"), expect(dead_code))]
    #[inline]
    pub(crate) fn motion(&self) -> Option<Motion> {
        self.reading.motion
    }
}

/// Which adaptive trigger an effect is for.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Trigger {
    Left,
    Right,
}

/// Force feedback of an adaptive trigger (DualSense). Positions run from 0 (released) to 1
/// (fully pulled); strengths and amplitudes from 0 to 1. Effects stay until replaced.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum TriggerEffect {
    /// No resistance.
    #[default]
    Off,
    /// Constant resistance from `start` to the end of travel (a bow string, a stiff pedal).
    Feedback { start: f32, strength: f32 },
    /// Resistance between `start` and `end` that gives way past `end`, like a gun's trigger.
    Weapon { start: f32, end: f32, strength: f32 },
    /// The trigger vibrates from `start` on, at `frequency` Hz.
    Vibration {
        start: f32,
        amplitude: f32,
        frequency: u8,
    },
    /// The controller's own 11-byte effect block, for effects not covered above.
    Raw([u8; 11]),
}

/// One actuator of a high-definition rumble motor (Switch Pro): two simultaneous tones. Amplitudes
/// in [0, 1]; frequencies are clamped to what the actuator plays (about 41–626 Hz for the low
/// tone, 82–1253 Hz for the high one).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct HapticTone {
    pub low_frequency: f32,
    pub low_amplitude: f32,
    pub high_frequency: f32,
    pub high_amplitude: f32,
}

impl HapticTone {
    /// Silence at the actuator's resting frequencies.
    pub const OFF: Self = Self {
        low_frequency: 160.0,
        low_amplitude: 0.0,
        high_frequency: 320.0,
        high_amplitude: 0.0,
    };
}

impl Default for HapticTone {
    fn default() -> Self {
        Self::OFF
    }
}
