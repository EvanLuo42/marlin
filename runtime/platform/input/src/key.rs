use core::fmt;

/// A physical key, identified by its USB HID usage (keyboard page 0x07).
///
/// Physical, not layout-dependent: `Key::W` is the key left of `E` on any layout, so WASD
/// bindings work on AZERTY too. Names follow the US layout; for the label printed on the user's
/// keyboard see [`Input::key_label`](crate::Input::key_label). Typed text comes from
/// [`InputState::text`](crate::InputState::text), never from keys.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(u8);

macro_rules! keys {
    ($($name:ident = $code:literal, $label:literal;)*) => {
        impl Key {
            $(pub const $name: Key = Key($code);)*

            /// The US-layout name ("A", "Left Shift", "Numpad 7"), or `None` for codes without
            /// one.
            pub const fn name(self) -> Option<&'static str> {
                match self.0 {
                    $($code => Some($label),)*
                    _ => None,
                }
            }
        }
    };
}

keys! {
    A = 0x04, "A"; B = 0x05, "B"; C = 0x06, "C"; D = 0x07, "D"; E = 0x08, "E"; F = 0x09, "F";
    G = 0x0A, "G"; H = 0x0B, "H"; I = 0x0C, "I"; J = 0x0D, "J"; K = 0x0E, "K"; L = 0x0F, "L";
    M = 0x10, "M"; N = 0x11, "N"; O = 0x12, "O"; P = 0x13, "P"; Q = 0x14, "Q"; R = 0x15, "R";
    S = 0x16, "S"; T = 0x17, "T"; U = 0x18, "U"; V = 0x19, "V"; W = 0x1A, "W"; X = 0x1B, "X";
    Y = 0x1C, "Y"; Z = 0x1D, "Z";
    DIGIT1 = 0x1E, "1"; DIGIT2 = 0x1F, "2"; DIGIT3 = 0x20, "3"; DIGIT4 = 0x21, "4";
    DIGIT5 = 0x22, "5"; DIGIT6 = 0x23, "6"; DIGIT7 = 0x24, "7"; DIGIT8 = 0x25, "8";
    DIGIT9 = 0x26, "9"; DIGIT0 = 0x27, "0";
    ENTER = 0x28, "Enter"; ESCAPE = 0x29, "Escape"; BACKSPACE = 0x2A, "Backspace";
    TAB = 0x2B, "Tab"; SPACE = 0x2C, "Space"; MINUS = 0x2D, "-"; EQUAL = 0x2E, "=";
    LEFT_BRACKET = 0x2F, "["; RIGHT_BRACKET = 0x30, "]"; BACKSLASH = 0x31, "\\";
    NON_US_HASH = 0x32, "#"; SEMICOLON = 0x33, ";"; APOSTROPHE = 0x34, "'"; GRAVE = 0x35, "`";
    COMMA = 0x36, ","; PERIOD = 0x37, "."; SLASH = 0x38, "/"; CAPS_LOCK = 0x39, "Caps Lock";
    F1 = 0x3A, "F1"; F2 = 0x3B, "F2"; F3 = 0x3C, "F3"; F4 = 0x3D, "F4"; F5 = 0x3E, "F5";
    F6 = 0x3F, "F6"; F7 = 0x40, "F7"; F8 = 0x41, "F8"; F9 = 0x42, "F9"; F10 = 0x43, "F10";
    F11 = 0x44, "F11"; F12 = 0x45, "F12";
    PRINT_SCREEN = 0x46, "Print Screen"; SCROLL_LOCK = 0x47, "Scroll Lock"; PAUSE = 0x48, "Pause";
    INSERT = 0x49, "Insert"; HOME = 0x4A, "Home"; PAGE_UP = 0x4B, "Page Up";
    DELETE = 0x4C, "Delete"; END = 0x4D, "End"; PAGE_DOWN = 0x4E, "Page Down";
    RIGHT = 0x4F, "Right"; LEFT = 0x50, "Left"; DOWN = 0x51, "Down"; UP = 0x52, "Up";
    NUM_LOCK = 0x53, "Num Lock"; NUMPAD_DIVIDE = 0x54, "Numpad /";
    NUMPAD_MULTIPLY = 0x55, "Numpad *"; NUMPAD_SUBTRACT = 0x56, "Numpad -";
    NUMPAD_ADD = 0x57, "Numpad +"; NUMPAD_ENTER = 0x58, "Numpad Enter";
    NUMPAD1 = 0x59, "Numpad 1"; NUMPAD2 = 0x5A, "Numpad 2"; NUMPAD3 = 0x5B, "Numpad 3";
    NUMPAD4 = 0x5C, "Numpad 4"; NUMPAD5 = 0x5D, "Numpad 5"; NUMPAD6 = 0x5E, "Numpad 6";
    NUMPAD7 = 0x5F, "Numpad 7"; NUMPAD8 = 0x60, "Numpad 8"; NUMPAD9 = 0x61, "Numpad 9";
    NUMPAD0 = 0x62, "Numpad 0"; NUMPAD_DECIMAL = 0x63, "Numpad .";
    NON_US_BACKSLASH = 0x64, "Non-US \\"; MENU = 0x65, "Menu"; NUMPAD_EQUAL = 0x67, "Numpad =";
    F13 = 0x68, "F13"; F14 = 0x69, "F14"; F15 = 0x6A, "F15"; F16 = 0x6B, "F16";
    F17 = 0x6C, "F17"; F18 = 0x6D, "F18"; F19 = 0x6E, "F19"; F20 = 0x6F, "F20";
    F21 = 0x70, "F21"; F22 = 0x71, "F22"; F23 = 0x72, "F23"; F24 = 0x73, "F24";
    NUMPAD_COMMA = 0x85, "Numpad ,"; INTL_RO = 0x87, "Ro"; KANA = 0x88, "Kana";
    INTL_YEN = 0x89, "Yen"; CONVERT = 0x8A, "Convert"; NON_CONVERT = 0x8B, "Non-Convert";
    LEFT_CTRL = 0xE0, "Left Ctrl"; LEFT_SHIFT = 0xE1, "Left Shift"; LEFT_ALT = 0xE2, "Left Alt";
    LEFT_META = 0xE3, "Left Meta"; RIGHT_CTRL = 0xE4, "Right Ctrl";
    RIGHT_SHIFT = 0xE5, "Right Shift"; RIGHT_ALT = 0xE6, "Right Alt";
    RIGHT_META = 0xE7, "Right Meta";
}

impl Key {
    /// Every key code; [`InputState`](crate::InputState) tracks this many keys.
    pub const COUNT: usize = 256;

    #[inline]
    pub const fn from_hid(usage: u8) -> Self {
        Self(usage)
    }

    #[inline]
    pub const fn hid(self) -> u8 {
        self.0
    }

    /// Looks a key up by its [`name`](Self::name), ignoring ASCII case. For key bindings in
    /// config files.
    pub fn from_name(name: &str) -> Option<Self> {
        (0..=u8::MAX)
            .map(Self)
            .find(|k| k.name().is_some_and(|n| n.eq_ignore_ascii_case(name)))
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => write!(f, "Key({name})"),
            None => write!(f, "Key({:#04x})", self.0),
        }
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "Key {:#04x}", self.0),
        }
    }
}
