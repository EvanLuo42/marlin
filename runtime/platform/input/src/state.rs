use marlin_app::WindowId;

use crate::{
    GamepadReading, GamepadState, Key, MAX_GAMEPADS, MAX_TOUCHES, PenSample, PenState, Touch,
    TouchPhase,
};

/// Bytes of typed text kept per frame; more is dropped and flagged.
pub const TEXT_CAPACITY: usize = 1024;

/// Bytes of IME composition text kept; longer compositions are truncated.
pub const COMPOSITION_CAPACITY: usize = 256;

/// Keyboards and mice told apart at once, see [`InputState::devices`].
pub const MAX_DEVICES: usize = 8;

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
    /// Relative motion straight from a device, before pointer ballistics. In device counts;
    /// positive y is down. For camera control. Adds to the combined delta and the device's.
    RawMotion {
        device: DeviceId,
        x: f32,
        y: f32,
    },
    /// A key of one particular keyboard; only that device's state changes ([`InputEvent::Key`]
    /// carries the combined view).
    DeviceKey {
        device: DeviceId,
        key: Key,
        down: bool,
    },
    /// A button of one particular mouse; only that device's state changes.
    DeviceButton {
        device: DeviceId,
        button: MouseButton,
        down: bool,
    },
    /// A keyboard or mouse was unplugged.
    DeviceRemoved(DeviceId),
    Touch {
        id: u32,
        window: WindowId,
        x: f32,
        y: f32,
        phase: TouchPhase,
    },
    Pen(PenSample),
    /// The pen left the detection range.
    PenLeft,
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

/// A keyboard or mouse as the OS enumerates it, to tell several apart (split-screen on one PC,
/// a dedicated macro pad). Stable while the device stays plugged in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DeviceId(u32);

impl DeviceId {
    /// Input that the OS does not attribute to a device (injected or remote input).
    pub const UNKNOWN: Self = Self(0);

    #[inline]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DeviceKind {
    Keyboard,
    Mouse,
}

/// One keyboard or mouse, as of this frame. Holds only what that device did.
#[derive(Clone, Copy, Debug)]
pub struct DeviceState {
    id: DeviceId,
    kind: DeviceKind,
    keys_down: KeySet,
    keys_pressed: KeySet,
    keys_released: KeySet,
    buttons_down: u8,
    buttons_pressed: u8,
    buttons_released: u8,
    delta: [f32; 2],
}

impl DeviceState {
    #[inline]
    pub fn id(&self) -> DeviceId {
        self.id
    }

    #[inline]
    pub fn kind(&self) -> DeviceKind {
        self.kind
    }

    #[inline]
    pub fn key_down(&self, key: Key) -> bool {
        self.keys_down.contains(key)
    }

    #[inline]
    pub fn key_pressed(&self, key: Key) -> bool {
        self.keys_pressed.contains(key)
    }

    #[inline]
    pub fn key_released(&self, key: Key) -> bool {
        self.keys_released.contains(key)
    }

    #[inline]
    pub fn button_down(&self, button: MouseButton) -> bool {
        self.buttons_down & bit(button) != 0
    }

    #[inline]
    pub fn button_pressed(&self, button: MouseButton) -> bool {
        self.buttons_pressed & bit(button) != 0
    }

    #[inline]
    pub fn button_released(&self, button: MouseButton) -> bool {
        self.buttons_released & bit(button) != 0
    }

    /// Raw motion of this mouse this frame (counts, +y down).
    #[inline]
    pub fn delta(&self) -> [f32; 2] {
        self.delta
    }

    fn begin_frame(&mut self) {
        self.keys_pressed = KeySet::default();
        self.keys_released = KeySet::default();
        self.buttons_pressed = 0;
        self.buttons_released = 0;
        self.delta = [0.0; 2];
    }

    fn release_all(&mut self) {
        self.keys_released.union_with(&self.keys_down);
        self.keys_down = KeySet::default();
        self.buttons_released |= self.buttons_down;
        self.buttons_down = 0;
    }
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

    fn union_with(&mut self, other: &KeySet) {
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a |= b;
        }
    }

    fn iter(&self) -> impl Iterator<Item = Key> + '_ {
        (0..Key::COUNT)
            .filter(|&i| self.0[i / 64] & (1 << (i % 64)) != 0)
            .map(|i| Key::from_hid(i as u8))
    }
}

/// UTF-8 in a fixed buffer.
#[derive(Clone, Copy)]
struct TextBuffer<const N: usize> {
    bytes: [u8; N],
    len: usize,
    truncated: bool,
}

impl<const N: usize> TextBuffer<N> {
    const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
            truncated: false,
        }
    }

    fn push(&mut self, c: char) {
        let n = c.len_utf8();
        if self.len + n > N {
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

/// Text being composed in an IME, before it is committed as [`InputState::text`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Composition<'a> {
    pub text: &'a str,
    /// The IME's caret, as a byte offset into `text`.
    pub cursor: usize,
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

    /// Raw device motion accumulated this frame over every mouse (counts, +y down). Unaffected
    /// by pointer speed settings, acceleration and screen edges, and still reported while the
    /// cursor is locked.
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
    text: TextBuffer<TEXT_CAPACITY>,
    composition: Option<(TextBuffer<COMPOSITION_CAPACITY>, usize)>,
    mouse: MouseState,
    devices: [Option<DeviceState>; MAX_DEVICES],
    touches: [Option<Touch>; MAX_TOUCHES],
    pen: PenState,
    gamepads: [GamepadState; MAX_GAMEPADS],
}

impl InputState {
    pub const fn new() -> Self {
        const EMPTY: KeySet = KeySet([0; 4]);
        Self {
            keys_down: EMPTY,
            keys_pressed: EMPTY,
            keys_released: EMPTY,
            keys_repeated: EMPTY,
            text: TextBuffer::new(),
            composition: None,
            mouse: MouseState {
                position: None,
                delta: [0.0; 2],
                wheel: [0.0; 2],
                down: 0,
                pressed: 0,
                released: 0,
            },
            devices: [None; MAX_DEVICES],
            touches: [None; MAX_TOUCHES],
            pen: PenState {
                sample: None,
                pressed: false,
                released: false,
            },
            gamepads: [GamepadState::DISCONNECTED; MAX_GAMEPADS],
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

    /// Text typed (or committed by an IME) this frame, in order.
    #[inline]
    pub fn text(&self) -> &str {
        self.text.as_str()
    }

    /// More than [`TEXT_CAPACITY`] bytes were typed this frame and the rest was dropped.
    #[inline]
    pub fn text_truncated(&self) -> bool {
        self.text.truncated
    }

    /// The IME composition in progress, to draw inline at the text cursor. Persists across
    /// frames until the IME commits it (to [`text`](Self::text)) or cancels it.
    pub fn composition(&self) -> Option<Composition<'_>> {
        self.composition.as_ref().map(|(text, cursor)| Composition {
            text: text.as_str(),
            cursor: *cursor,
        })
    }

    #[inline]
    pub fn mouse(&self) -> &MouseState {
        &self.mouse
    }

    /// The keyboards and mice that produced input since they were connected, in first-use order.
    pub fn devices(&self) -> impl Iterator<Item = &DeviceState> + '_ {
        self.devices.iter().flatten()
    }

    pub fn device(&self, id: DeviceId) -> Option<&DeviceState> {
        self.devices().find(|d| d.id == id)
    }

    /// Fingers down this frame, plus those lifted this frame (with `released` set).
    pub fn touches(&self) -> impl Iterator<Item = &Touch> + '_ {
        self.touches.iter().flatten()
    }

    #[inline]
    pub fn pen(&self) -> &PenState {
        &self.pen
    }

    /// Panics if `index >= MAX_GAMEPADS`.
    #[inline]
    pub fn gamepad(&self, index: usize) -> &GamepadState {
        &self.gamepads[index]
    }

    pub fn gamepads(&self) -> &[GamepadState; MAX_GAMEPADS] {
        &self.gamepads
    }

    /// Starts a new frame: clears edges, text and deltas, and forgets lifted touches; keeps what
    /// is held and the composition.
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
        for device in self.devices.iter_mut().flatten() {
            device.begin_frame();
        }
        for slot in &mut self.touches {
            match slot {
                Some(touch) if touch.released => *slot = None,
                Some(touch) => touch.pressed = false,
                None => {}
            }
        }
        self.pen.pressed = false;
        self.pen.released = false;
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
                let mouse = &mut self.mouse;
                press(
                    &mut mouse.down,
                    &mut mouse.pressed,
                    &mut mouse.released,
                    button,
                    down,
                );
            }
            InputEvent::Wheel { x, y } => {
                self.mouse.wheel[0] += x;
                self.mouse.wheel[1] += y;
            }
            InputEvent::RawMotion { device, x, y } => {
                self.mouse.delta[0] += x;
                self.mouse.delta[1] += y;
                if let Some(d) = self.device_mut(device, DeviceKind::Mouse) {
                    d.delta[0] += x;
                    d.delta[1] += y;
                }
            }
            InputEvent::DeviceKey { device, key, down } => {
                if let Some(d) = self.device_mut(device, DeviceKind::Keyboard) {
                    if down {
                        if !d.keys_down.contains(key) {
                            d.keys_pressed.set(key, true);
                        }
                    } else {
                        if !d.keys_down.contains(key) {
                            d.keys_pressed.set(key, true);
                        }
                        d.keys_released.set(key, true);
                    }
                    d.keys_down.set(key, down);
                }
            }
            InputEvent::DeviceButton {
                device,
                button,
                down,
            } => {
                if let Some(d) = self.device_mut(device, DeviceKind::Mouse) {
                    press(
                        &mut d.buttons_down,
                        &mut d.buttons_pressed,
                        &mut d.buttons_released,
                        button,
                        down,
                    );
                }
            }
            InputEvent::DeviceRemoved(device) => {
                for slot in &mut self.devices {
                    if slot.is_some_and(|d| d.id == device) {
                        *slot = None;
                    }
                }
            }
            InputEvent::Touch {
                id,
                window,
                x,
                y,
                phase,
            } => self.touch(id, window, [x, y], phase),
            InputEvent::Pen(sample) => {
                let was_down = self.pen.down();
                if sample.contact && !was_down {
                    self.pen.pressed = true;
                } else if !sample.contact && was_down {
                    self.pen.released = true;
                }
                self.pen.sample = Some(sample);
            }
            InputEvent::PenLeft => {
                if self.pen.down() {
                    self.pen.released = true;
                }
                self.pen.sample = None;
            }
            InputEvent::FocusLost => {
                self.keys_released.union_with(&self.keys_down);
                self.keys_down = KeySet::default();
                self.mouse.released |= self.mouse.down;
                self.mouse.down = 0;
                for device in self.devices.iter_mut().flatten() {
                    device.release_all();
                }
                for touch in self.touches.iter_mut().flatten() {
                    touch.released = true;
                    touch.cancelled = true;
                }
                if self.pen.down() {
                    self.pen.released = true;
                }
                self.pen.sample = None;
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

    /// Sets the IME composition in progress (`cursor` is a byte offset, clamped to a char
    /// boundary), or clears it with `None`. Text beyond [`COMPOSITION_CAPACITY`] is cut.
    pub fn set_composition(&mut self, composition: Option<(&str, usize)>) {
        self.composition = composition.map(|(text, cursor)| {
            let mut buffer = TextBuffer::new();
            for c in text.chars() {
                buffer.push(c);
            }
            let s = buffer.as_str();
            let mut cursor = cursor.min(s.len());
            while !s.is_char_boundary(cursor) {
                cursor -= 1;
            }
            (buffer, cursor)
        });
    }

    /// The state slot of `id`, allocated on first use. `None` when every slot is taken.
    fn device_mut(&mut self, id: DeviceId, kind: DeviceKind) -> Option<&mut DeviceState> {
        let index = match self
            .devices
            .iter()
            .position(|d| d.is_some_and(|d| d.id == id))
        {
            Some(i) => i,
            None => {
                let i = self.devices.iter().position(Option::is_none)?;
                self.devices[i] = Some(DeviceState {
                    id,
                    kind,
                    keys_down: KeySet::default(),
                    keys_pressed: KeySet::default(),
                    keys_released: KeySet::default(),
                    buttons_down: 0,
                    buttons_pressed: 0,
                    buttons_released: 0,
                    delta: [0.0; 2],
                });
                i
            }
        };
        self.devices[index].as_mut()
    }

    fn touch(&mut self, id: u32, window: WindowId, position: [f32; 2], phase: TouchPhase) {
        // A finger lifted this frame keeps its slot until `begin_frame`, so a new touch reusing
        // its id starts fresh.
        let slot = self
            .touches
            .iter()
            .position(|t| t.is_some_and(|t| t.id == id && !t.released));
        match (slot, phase) {
            (None, TouchPhase::Started | TouchPhase::Moved) => {
                if let Some(free) = self.touches.iter().position(Option::is_none) {
                    self.touches[free] = Some(Touch {
                        id,
                        window,
                        position,
                        pressed: true,
                        released: false,
                        cancelled: false,
                    });
                }
            }
            (None, _) => {}
            (Some(i), _) => {
                let touch = self.touches[i].as_mut().expect("occupied");
                touch.window = window;
                touch.position = position;
                match phase {
                    TouchPhase::Started | TouchPhase::Moved => {}
                    TouchPhase::Ended => touch.released = true,
                    TouchPhase::Cancelled => {
                        touch.released = true;
                        touch.cancelled = true;
                    }
                }
            }
        }
    }
}

/// Applies a button edge to `down` / `pressed` / `released` bit sets.
fn press(down: &mut u8, pressed: &mut u8, released: &mut u8, button: MouseButton, is_down: bool) {
    let b = bit(button);
    if is_down {
        *pressed |= b;
        *down |= b;
    } else {
        if *down & b == 0 {
            *pressed |= b;
        }
        *released |= b;
        *down &= !b;
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
            .field("composition", &self.composition())
            .field("mouse", &self.mouse)
            .field("touches", &self.touches().count())
            .field("pen", &self.pen)
            .field("gamepads", &self.gamepads)
            .finish_non_exhaustive()
    }
}

impl core::fmt::Debug for KeySet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}
