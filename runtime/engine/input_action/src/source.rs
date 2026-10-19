//! Raw input sources ([`InputSource`]: a key, a button, a stick) and one frame of their values
//! for one input user ([`InputFrame`]), which routing can strip of what a higher layer took.

use alloc::string::String;
use core::fmt;

use marlin_input::{GamepadButton, InputState, Key, MouseButton};
use marlin_math::Vec3;
use marlin_reflect::{Proxied, Reflect, ReflectProxy, TypeInfo, TypeKind, TypeName};

/// A keyboard key, written by its US-layout name (`"W"`, `"Left Shift"`, `"F11"`; see
/// `marlin_input::Key::name`). An unknown name is an error where the data is read.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct KeyCode(pub Key);

impl Default for KeyCode {
    fn default() -> Self {
        Self(Key::SPACE)
    }
}

impl Proxied for KeyCode {
    type Proxy = String;

    fn to_proxy(&self) -> String {
        String::from(self.0.name().unwrap_or("?"))
    }

    fn set_from_proxy(&mut self, name: &String) -> bool {
        match Key::from_name(name) {
            Some(key) => {
                self.0 = key;
                true
            }
            None => false,
        }
    }
}

// SAFETY: an opaque description of `KeyCode` itself.
unsafe impl Reflect for KeyCode {
    const TYPE_NAME: TypeName = TypeName::simple("marlin_input_action::KeyCode");
    const TYPE_INFO: TypeInfo = TypeInfo {
        data: &[&ReflectProxy::of::<Self>()],
        ..TypeInfo::new::<Self>(TypeKind::Opaque)
    };

    fn reflect_default() -> Option<Self> {
        Some(Self::default())
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(*self)
    }
}

/// A mouse button (`marlin_input::MouseButton`, reflected).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Reflect)]
#[reflect(name = "marlin_input_action::Mouse", default)]
pub enum Mouse {
    #[default]
    Left,
    Right,
    Middle,
    X1,
    X2,
}

impl Mouse {
    const ALL: [Mouse; 5] = [Self::Left, Self::Right, Self::Middle, Self::X1, Self::X2];

    fn button(self) -> MouseButton {
        match self {
            Self::Left => MouseButton::Left,
            Self::Right => MouseButton::Right,
            Self::Middle => MouseButton::Middle,
            Self::X1 => MouseButton::X1,
            Self::X2 => MouseButton::X2,
        }
    }

    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// A gamepad button by position (`marlin_input::GamepadButton`, reflected).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Reflect)]
#[reflect(name = "marlin_input_action::Pad", default)]
pub enum Pad {
    #[default]
    South,
    East,
    West,
    North,
    LeftShoulder,
    RightShoulder,
    LeftTrigger,
    RightTrigger,
    Select,
    Start,
    LeftStick,
    RightStick,
    DPadUp,
    DPadDown,
    DPadLeft,
    DPadRight,
    Home,
    Touchpad,
    Misc,
}

impl Pad {
    fn button(self) -> GamepadButton {
        match self {
            Self::South => GamepadButton::South,
            Self::East => GamepadButton::East,
            Self::West => GamepadButton::West,
            Self::North => GamepadButton::North,
            Self::LeftShoulder => GamepadButton::LeftShoulder,
            Self::RightShoulder => GamepadButton::RightShoulder,
            Self::LeftTrigger => GamepadButton::LeftTrigger,
            Self::RightTrigger => GamepadButton::RightTrigger,
            Self::Select => GamepadButton::Select,
            Self::Start => GamepadButton::Start,
            Self::LeftStick => GamepadButton::LeftStick,
            Self::RightStick => GamepadButton::RightStick,
            Self::DPadUp => GamepadButton::DPadUp,
            Self::DPadDown => GamepadButton::DPadDown,
            Self::DPadLeft => GamepadButton::DPadLeft,
            Self::DPadRight => GamepadButton::DPadRight,
            Self::Home => GamepadButton::Home,
            Self::Touchpad => GamepadButton::Touchpad,
            Self::Misc => GamepadButton::Misc,
        }
    }
}

/// Where a mapping reads its value: digital sources give 1 on x while down; sticks, mouse
/// motion and the wheel give x and y; triggers give x in [0, 1].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Reflect)]
#[reflect(name = "marlin_input_action::InputSource", default)]
pub enum InputSource {
    #[default]
    None,
    Key(KeyCode),
    Mouse(Mouse),
    /// This frame's mouse motion, in pixels (+x right, +y down).
    MouseMotion,
    /// This frame's wheel motion, in notches (+y away from the user).
    MouseWheel,
    Pad(Pad),
    /// [-1, 1] each, +y up.
    LeftStick,
    RightStick,
    /// [0, 1].
    LeftTrigger,
    RightTrigger,
}

impl fmt::Display for InputSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(k) => write!(f, "{}", k.0),
            other => write!(f, "{other:?}"),
        }
    }
}

/// Which devices an input user reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Devices {
    pub keyboard_mouse: bool,
    /// A gamepad slot (`marlin_input::InputState::gamepad`).
    pub gamepad: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
struct PadFrame {
    buttons: u32,
    left: [f32; 2],
    right: [f32; 2],
    left_trigger: f32,
    right_trigger: f32,
}

/// One frame of raw input for one user: what each source reads. A button that went down and
/// up within the frame reads down, so short taps are never lost.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct InputFrame {
    keys: [u64; 4],
    mouse: u8,
    motion: [f32; 2],
    wheel: [f32; 2],
    pad: Option<PadFrame>,
}

impl InputFrame {
    /// Nothing down, nothing moving.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The sources of `devices` in `state`.
    pub fn from_state(state: &InputState, devices: Devices) -> Self {
        let mut frame = Self::default();
        if devices.keyboard_mouse {
            for code in 0..=u8::MAX {
                let key = Key::from_hid(code);
                if state.key_down(key) || state.key_pressed(key) {
                    frame.keys[usize::from(code >> 6)] |= 1 << (code & 63);
                }
            }
            let mouse = state.mouse();
            for b in Mouse::ALL {
                if mouse.down(b.button()) || mouse.pressed(b.button()) {
                    frame.mouse |= b.bit();
                }
            }
            frame.motion = mouse.delta();
            frame.wheel = mouse.wheel();
        }
        if let Some(index) = devices.gamepad {
            let pad = state.gamepad(index);
            if pad.is_connected() {
                let mut buttons = 0;
                for p in PAD_ALL {
                    let b = p.button();
                    if pad.down(b) || pad.pressed(b) {
                        buttons |= b.bit();
                    }
                }
                frame.pad = Some(PadFrame {
                    buttons,
                    left: pad.left_stick(),
                    right: pad.right_stick(),
                    left_trigger: pad.left_trigger(),
                    right_trigger: pad.right_trigger(),
                });
            }
        }
        frame
    }

    pub fn key(&self, key: Key) -> bool {
        let code = key.hid();
        self.keys[usize::from(code >> 6)] & (1 << (code & 63)) != 0
    }

    /// What `source` reads this frame.
    pub fn read(&self, source: &InputSource) -> Vec3 {
        let flag = |on: bool| Vec3::new(if on { 1.0 } else { 0.0 }, 0.0, 0.0);
        let xy = |v: [f32; 2]| Vec3::new(v[0], v[1], 0.0);
        let pad = self.pad.unwrap_or_default();
        match *source {
            InputSource::None => Vec3::ZERO,
            InputSource::Key(k) => flag(self.key(k.0)),
            InputSource::Mouse(b) => flag(self.mouse & b.bit() != 0),
            InputSource::MouseMotion => xy(self.motion),
            InputSource::MouseWheel => xy(self.wheel),
            InputSource::Pad(b) => flag(pad.buttons & b.button().bit() != 0),
            InputSource::LeftStick => xy(pad.left),
            InputSource::RightStick => xy(pad.right),
            InputSource::LeftTrigger => Vec3::new(pad.left_trigger, 0.0, 0.0),
            InputSource::RightTrigger => Vec3::new(pad.right_trigger, 0.0, 0.0),
        }
    }

    /// Takes `source` away: it reads nothing from now on (a higher layer used it).
    pub fn consume(&mut self, source: &InputSource) {
        match *source {
            InputSource::None => {}
            InputSource::Key(k) => {
                let code = k.0.hid();
                self.keys[usize::from(code >> 6)] &= !(1 << (code & 63));
            }
            InputSource::Mouse(b) => self.mouse &= !b.bit(),
            InputSource::MouseMotion => self.motion = [0.0; 2],
            InputSource::MouseWheel => self.wheel = [0.0; 2],
            InputSource::Pad(b) => {
                if let Some(p) = &mut self.pad {
                    p.buttons &= !b.button().bit();
                }
            }
            InputSource::LeftStick => {
                if let Some(p) = &mut self.pad {
                    p.left = [0.0; 2];
                }
            }
            InputSource::RightStick => {
                if let Some(p) = &mut self.pad {
                    p.right = [0.0; 2];
                }
            }
            InputSource::LeftTrigger => {
                if let Some(p) = &mut self.pad {
                    p.left_trigger = 0.0;
                }
            }
            InputSource::RightTrigger => {
                if let Some(p) = &mut self.pad {
                    p.right_trigger = 0.0;
                }
            }
        }
    }

    /// The keyboard taken away (a text field has the focus).
    pub fn without_keyboard(mut self) -> Self {
        self.keys = [0; 4];
        self
    }

    /// The mouse taken away (the pointer is over UI).
    pub fn without_mouse(mut self) -> Self {
        self.mouse = 0;
        self.motion = [0.0; 2];
        self.wheel = [0.0; 2];
        self
    }
}

const PAD_ALL: [Pad; 19] = [
    Pad::South,
    Pad::East,
    Pad::West,
    Pad::North,
    Pad::LeftShoulder,
    Pad::RightShoulder,
    Pad::LeftTrigger,
    Pad::RightTrigger,
    Pad::Select,
    Pad::Start,
    Pad::LeftStick,
    Pad::RightStick,
    Pad::DPadUp,
    Pad::DPadDown,
    Pad::DPadLeft,
    Pad::DPadRight,
    Pad::Home,
    Pad::Touchpad,
    Pad::Misc,
];

/// Builders for tests and tools: a frame with given sources set.
impl InputFrame {
    pub fn with_key(mut self, key: Key) -> Self {
        let code = key.hid();
        self.keys[usize::from(code >> 6)] |= 1 << (code & 63);
        self
    }

    pub fn with_mouse(mut self, button: Mouse) -> Self {
        self.mouse |= button.bit();
        self
    }

    pub fn with_motion(mut self, motion: [f32; 2]) -> Self {
        self.motion = motion;
        self
    }

    pub fn with_pad(mut self, button: Pad) -> Self {
        let p = self.pad.get_or_insert_with(PadFrame::default);
        p.buttons |= button.button().bit();
        self
    }

    pub fn with_sticks(mut self, left: [f32; 2], right: [f32; 2]) -> Self {
        let p = self.pad.get_or_insert_with(PadFrame::default);
        p.left = left;
        p.right = right;
        self
    }
}
