use marlin_app::WindowId;

use crate::Key;

/// Gamepad slots; XInput's limit, and enough for local multiplayer on every target.
pub const MAX_GAMEPADS: usize = 4;

/// Bytes of typed text kept per frame; more is dropped and flagged.
pub const TEXT_CAPACITY: usize = 128;

/// A platform-neutral input change. Backends translate OS messages into these; tests, replays
/// and virtual keyboards can feed them too.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum InputEvent {
    /// `repeat`: the OS reports auto-repeat of a key that is already down.
    Key {
        key: Key,
        down: bool,
        repeat: bool,
    },
    /// A typed character (already composed by the layout and IME). Control characters are
    /// ignored: Enter and Backspace are keys.
    Text(char),
    /// Cursor position in the window's client area, in physical pixels.
    MouseMoved {
        window: WindowId,
        x: f32,
        y: f32,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
    },
    /// Wheel movement in notches (detents); fractional for high-resolution wheels. Positive is
    /// up / right.
    Wheel {
        x: f32,
        y: f32,
    },
    /// Relative motion straight from the device, before pointer ballistics. In device counts;
    /// positive y is down. For camera control.
    RawMotion {
        x: f32,
        y: f32,
    },
    /// The application lost keyboard focus: everything held is released.
    FocusLost,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// Usually "back".
    X1,
    /// Usually "forward".
    X2,
}

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
    /// View / Back / Share.
    Select,
    /// Menu / Start / Options.
    Start,
    LeftStick,
    RightStick,
    DPadUp,
    DPadDown,
    DPadLeft,
    DPadRight,
}

impl GamepadButton {
    #[inline]
    pub const fn bit(self) -> u32 {
        1 << self as u8
    }
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

/// A set of keys: one bit per HID usage.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct KeySet([u64; 4]);

impl KeySet {
    #[inline]
    fn contains(&self, key: Key) -> bool {
        let i = key.hid() as usize;
        self.0[i / 64] & (1 << (i % 64)) != 0
    }

    #[inline]
    fn set(&mut self, key: Key, value: bool) {
        let i = key.hid() as usize;
        if value {
            self.0[i / 64] |= 1 << (i % 64);
        } else {
            self.0[i / 64] &= !(1 << (i % 64));
        }
    }

    fn iter(&self) -> impl Iterator<Item = Key> + '_ {
        (0..Key::COUNT)
            .filter(|&i| self.0[i / 64] & (1 << (i % 64)) != 0)
            .map(|i| Key::from_hid(i as u8))
    }
}

/// UTF-8 typed during one frame.
#[derive(Clone, Copy)]
struct TextBuffer {
    bytes: [u8; TEXT_CAPACITY],
    len: usize,
    truncated: bool,
}

impl TextBuffer {
    const fn new() -> Self {
        Self {
            bytes: [0; TEXT_CAPACITY],
            len: 0,
            truncated: false,
        }
    }

    fn push(&mut self, c: char) {
        let n = c.len_utf8();
        if self.len + n > TEXT_CAPACITY {
            self.truncated = true;
            return;
        }
        c.encode_utf8(&mut self.bytes[self.len..]);
        self.len += n;
    }

    fn as_str(&self) -> &str {
        // SAFETY: only whole chars are ever encoded into `bytes[..len]`.
        unsafe { core::str::from_utf8_unchecked(&self.bytes[..self.len]) }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MouseState {
    position: Option<(WindowId, [f32; 2])>,
    delta: [f32; 2],
    wheel: [f32; 2],
    down: u8,
    pressed: u8,
    released: u8,
}

impl MouseState {
    /// Last cursor position, in physical pixels of the window's client area. `None` until the
    /// cursor first moves over a window.
    #[inline]
    pub fn position(&self) -> Option<[f32; 2]> {
        self.position.map(|(_, p)| p)
    }

    /// The window [`position`](Self::position) refers to.
    #[inline]
    pub fn window(&self) -> Option<WindowId> {
        self.position.map(|(w, _)| w)
    }

    /// Raw device motion accumulated this frame (counts, +y down). Unaffected by pointer speed
    /// settings, acceleration and screen edges.
    #[inline]
    pub fn delta(&self) -> [f32; 2] {
        self.delta
    }

    /// Wheel notches this frame (+y up, +x right).
    #[inline]
    pub fn wheel(&self) -> [f32; 2] {
        self.wheel
    }

    #[inline]
    pub fn down(&self, button: MouseButton) -> bool {
        self.down & bit(button) != 0
    }

    /// Went down at least once this frame.
    #[inline]
    pub fn pressed(&self, button: MouseButton) -> bool {
        self.pressed & bit(button) != 0
    }

    /// Went up at least once this frame.
    #[inline]
    pub fn released(&self, button: MouseButton) -> bool {
        self.released & bit(button) != 0
    }
}

fn bit(button: MouseButton) -> u8 {
    1 << button as u8
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GamepadState {
    connected: bool,
    reading: GamepadReading,
    pressed: u32,
    released: u32,
}

impl GamepadState {
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.connected
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
}

/// The input of one frame: what is held, plus the edges (pressed / released) and deltas since the
/// previous frame.
///
/// [`Input::update`](crate::Input::update) produces one per frame. Edges are counted, not
/// sampled: a key tapped within one frame reports both `pressed` and `released` while `down` is
/// false, so short taps are never lost at low frame rates.
#[derive(Clone, Copy)]
pub struct InputState {
    keys_down: KeySet,
    keys_pressed: KeySet,
    keys_released: KeySet,
    keys_repeated: KeySet,
    text: TextBuffer,
    mouse: MouseState,
    gamepads: [GamepadState; MAX_GAMEPADS],
}

impl InputState {
    pub const fn new() -> Self {
        const EMPTY: KeySet = KeySet([0; 4]);
        const PAD: GamepadState = GamepadState {
            connected: false,
            reading: GamepadReading {
                buttons: 0,
                left_stick: [0.0; 2],
                right_stick: [0.0; 2],
                left_trigger: 0.0,
                right_trigger: 0.0,
            },
            pressed: 0,
            released: 0,
        };
        Self {
            keys_down: EMPTY,
            keys_pressed: EMPTY,
            keys_released: EMPTY,
            keys_repeated: EMPTY,
            text: TextBuffer::new(),
            mouse: MouseState {
                position: None,
                delta: [0.0; 2],
                wheel: [0.0; 2],
                down: 0,
                pressed: 0,
                released: 0,
            },
            gamepads: [PAD; MAX_GAMEPADS],
        }
    }

    #[inline]
    pub fn key_down(&self, key: Key) -> bool {
        self.keys_down.contains(key)
    }

    /// Went down at least once this frame (auto-repeat excluded).
    #[inline]
    pub fn key_pressed(&self, key: Key) -> bool {
        self.keys_pressed.contains(key)
    }

    /// Went up at least once this frame.
    #[inline]
    pub fn key_released(&self, key: Key) -> bool {
        self.keys_released.contains(key)
    }

    /// Pressed or auto-repeated this frame: for UI navigation that repeats while held.
    #[inline]
    pub fn key_pressed_or_repeated(&self, key: Key) -> bool {
        self.keys_pressed.contains(key) || self.keys_repeated.contains(key)
    }

    pub fn keys_down(&self) -> impl Iterator<Item = Key> + '_ {
        self.keys_down.iter()
    }

    #[inline]
    pub fn shift(&self) -> bool {
        self.key_down(Key::LEFT_SHIFT) || self.key_down(Key::RIGHT_SHIFT)
    }

    #[inline]
    pub fn ctrl(&self) -> bool {
        self.key_down(Key::LEFT_CTRL) || self.key_down(Key::RIGHT_CTRL)
    }

    #[inline]
    pub fn alt(&self) -> bool {
        self.key_down(Key::LEFT_ALT) || self.key_down(Key::RIGHT_ALT)
    }

    /// Text typed this frame, in order.
    #[inline]
    pub fn text(&self) -> &str {
        self.text.as_str()
    }

    /// More than [`TEXT_CAPACITY`] bytes were typed this frame and the rest was dropped.
    #[inline]
    pub fn text_truncated(&self) -> bool {
        self.text.truncated
    }

    #[inline]
    pub fn mouse(&self) -> &MouseState {
        &self.mouse
    }

    /// Panics if `index >= MAX_GAMEPADS`.
    #[inline]
    pub fn gamepad(&self, index: usize) -> &GamepadState {
        &self.gamepads[index]
    }

    pub fn gamepads(&self) -> &[GamepadState; MAX_GAMEPADS] {
        &self.gamepads
    }

    /// Starts a new frame: clears edges, text and deltas, keeps what is held.
    pub fn begin_frame(&mut self) {
        self.keys_pressed = KeySet::default();
        self.keys_released = KeySet::default();
        self.keys_repeated = KeySet::default();
        self.text = TextBuffer::new();
        let mouse = &mut self.mouse;
        mouse.delta = [0.0; 2];
        mouse.wheel = [0.0; 2];
        mouse.pressed = 0;
        mouse.released = 0;
        for pad in &mut self.gamepads {
            pad.pressed = 0;
            pad.released = 0;
        }
    }

    /// Applies one event to the current frame.
    pub fn apply(&mut self, event: InputEvent) {
        match event {
            InputEvent::Key {
                key, down: true, ..
            } => {
                if self.keys_down.contains(key) {
                    // A down for a held key is a repeat even if the OS did not flag it.
                    self.keys_repeated.set(key, true);
                } else {
                    self.keys_down.set(key, true);
                    self.keys_pressed.set(key, true);
                }
            }
            InputEvent::Key {
                key, down: false, ..
            } => {
                // Some keys (Print Screen on Windows) only report the release; count the press too.
                if !self.keys_down.contains(key) {
                    self.keys_pressed.set(key, true);
                }
                self.keys_down.set(key, false);
                self.keys_released.set(key, true);
            }
            InputEvent::Text(c) => {
                if !c.is_control() {
                    self.text.push(c);
                }
            }
            InputEvent::MouseMoved { window, x, y } => self.mouse.position = Some((window, [x, y])),
            InputEvent::MouseButton { button, down } => {
                let b = bit(button);
                if down {
                    self.mouse.pressed |= b;
                    self.mouse.down |= b;
                } else {
                    if self.mouse.down & b == 0 {
                        self.mouse.pressed |= b;
                    }
                    self.mouse.released |= b;
                    self.mouse.down &= !b;
                }
            }
            InputEvent::Wheel { x, y } => {
                self.mouse.wheel[0] += x;
                self.mouse.wheel[1] += y;
            }
            InputEvent::RawMotion { x, y } => {
                self.mouse.delta[0] += x;
                self.mouse.delta[1] += y;
            }
            InputEvent::FocusLost => {
                for (released, down) in self.keys_released.0.iter_mut().zip(&self.keys_down.0) {
                    *released |= down;
                }
                self.keys_down = KeySet::default();
                self.mouse.released |= self.mouse.down;
                self.mouse.down = 0;
            }
        }
    }

    /// Records a gamepad poll (`None`: disconnected). Edges are relative to the previous reading.
    /// Panics if `index >= MAX_GAMEPADS`.
    pub fn set_gamepad(&mut self, index: usize, reading: Option<&GamepadReading>) {
        let pad = &mut self.gamepads[index];
        let new = reading.copied().unwrap_or_default();
        let old = pad.reading.buttons;
        pad.pressed |= new.buttons & !old;
        pad.released |= old & !new.buttons;
        pad.reading = new;
        pad.connected = reading.is_some();
    }
}

impl Default for InputState {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for InputState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InputState")
            .field("keys_down", &self.keys_down)
            .field("text", &self.text())
            .field("mouse", &self.mouse)
            .field("gamepads", &self.gamepads)
            .finish_non_exhaustive()
    }
}

impl core::fmt::Debug for KeySet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}
