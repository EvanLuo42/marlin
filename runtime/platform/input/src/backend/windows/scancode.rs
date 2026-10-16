//! PC/AT scan code set 1 (what Windows reports in key message `lParam`s) to HID usages.

use crate::Key;

const fn table(pairs: &[(u8, Key)]) -> [u8; 128] {
    let mut t = [0; 128];
    let mut i = 0;
    while i < pairs.len() {
        t[pairs[i].0 as usize] = pairs[i].1.hid();
        i += 1;
    }
    t
}

/// Codes without the E0 prefix.
const PLAIN: [u8; 128] = table(&[
    (0x01, Key::ESCAPE),
    (0x02, Key::DIGIT1),
    (0x03, Key::DIGIT2),
    (0x04, Key::DIGIT3),
    (0x05, Key::DIGIT4),
    (0x06, Key::DIGIT5),
    (0x07, Key::DIGIT6),
    (0x08, Key::DIGIT7),
    (0x09, Key::DIGIT8),
    (0x0A, Key::DIGIT9),
    (0x0B, Key::DIGIT0),
    (0x0C, Key::MINUS),
    (0x0D, Key::EQUAL),
    (0x0E, Key::BACKSPACE),
    (0x0F, Key::TAB),
    (0x10, Key::Q),
    (0x11, Key::W),
    (0x12, Key::E),
    (0x13, Key::R),
    (0x14, Key::T),
    (0x15, Key::Y),
    (0x16, Key::U),
    (0x17, Key::I),
    (0x18, Key::O),
    (0x19, Key::P),
    (0x1A, Key::LEFT_BRACKET),
    (0x1B, Key::RIGHT_BRACKET),
    (0x1C, Key::ENTER),
    (0x1D, Key::LEFT_CTRL),
    (0x1E, Key::A),
    (0x1F, Key::S),
    (0x20, Key::D),
    (0x21, Key::F),
    (0x22, Key::G),
    (0x23, Key::H),
    (0x24, Key::J),
    (0x25, Key::K),
    (0x26, Key::L),
    (0x27, Key::SEMICOLON),
    (0x28, Key::APOSTROPHE),
    (0x29, Key::GRAVE),
    (0x2A, Key::LEFT_SHIFT),
    (0x2B, Key::BACKSLASH),
    (0x2C, Key::Z),
    (0x2D, Key::X),
    (0x2E, Key::C),
    (0x2F, Key::V),
    (0x30, Key::B),
    (0x31, Key::N),
    (0x32, Key::M),
    (0x33, Key::COMMA),
    (0x34, Key::PERIOD),
    (0x35, Key::SLASH),
    (0x36, Key::RIGHT_SHIFT),
    (0x37, Key::NUMPAD_MULTIPLY),
    (0x38, Key::LEFT_ALT),
    (0x39, Key::SPACE),
    (0x3A, Key::CAPS_LOCK),
    (0x3B, Key::F1),
    (0x3C, Key::F2),
    (0x3D, Key::F3),
    (0x3E, Key::F4),
    (0x3F, Key::F5),
    (0x40, Key::F6),
    (0x41, Key::F7),
    (0x42, Key::F8),
    (0x43, Key::F9),
    (0x44, Key::F10),
    // Pause also reports 0x45; told apart by virtual key in `key_from_message`.
    (0x45, Key::NUM_LOCK),
    (0x46, Key::SCROLL_LOCK),
    (0x47, Key::NUMPAD7),
    (0x48, Key::NUMPAD8),
    (0x49, Key::NUMPAD9),
    (0x4A, Key::NUMPAD_SUBTRACT),
    (0x4B, Key::NUMPAD4),
    (0x4C, Key::NUMPAD5),
    (0x4D, Key::NUMPAD6),
    (0x4E, Key::NUMPAD_ADD),
    (0x4F, Key::NUMPAD1),
    (0x50, Key::NUMPAD2),
    (0x51, Key::NUMPAD3),
    (0x52, Key::NUMPAD0),
    (0x53, Key::NUMPAD_DECIMAL),
    // Alt+Print Screen.
    (0x54, Key::PRINT_SCREEN),
    (0x56, Key::NON_US_BACKSLASH),
    (0x57, Key::F11),
    (0x58, Key::F12),
    (0x59, Key::NUMPAD_EQUAL),
    (0x64, Key::F13),
    (0x65, Key::F14),
    (0x66, Key::F15),
    (0x67, Key::F16),
    (0x68, Key::F17),
    (0x69, Key::F18),
    (0x6A, Key::F19),
    (0x6B, Key::F20),
    (0x6C, Key::F21),
    (0x6D, Key::F22),
    (0x6E, Key::F23),
    (0x70, Key::KANA),
    (0x73, Key::INTL_RO),
    (0x76, Key::F24),
    (0x79, Key::CONVERT),
    (0x7B, Key::NON_CONVERT),
    (0x7D, Key::INTL_YEN),
    (0x7E, Key::NUMPAD_COMMA),
]);

/// Codes with the E0 prefix (`KF_EXTENDED`). The fake shifts some keyboards send around
/// navigation keys (E0 2A / E0 36) are deliberately absent.
const EXTENDED: [u8; 128] = table(&[
    (0x1C, Key::NUMPAD_ENTER),
    (0x1D, Key::RIGHT_CTRL),
    (0x35, Key::NUMPAD_DIVIDE),
    (0x37, Key::PRINT_SCREEN),
    (0x38, Key::RIGHT_ALT),
    (0x45, Key::NUM_LOCK),
    // Ctrl+Pause (Break).
    (0x46, Key::PAUSE),
    (0x47, Key::HOME),
    (0x48, Key::UP),
    (0x49, Key::PAGE_UP),
    (0x4B, Key::LEFT),
    (0x4D, Key::RIGHT),
    (0x4F, Key::END),
    (0x50, Key::DOWN),
    (0x51, Key::PAGE_DOWN),
    (0x52, Key::INSERT),
    (0x53, Key::DELETE),
    (0x5B, Key::LEFT_META),
    (0x5C, Key::RIGHT_META),
    (0x5D, Key::MENU),
]);

/// The key for scan code `code` (low 7 bits) with or without the E0 prefix.
pub(super) fn to_key(code: u8, extended: bool) -> Option<Key> {
    let table = if extended { &EXTENDED } else { &PLAIN };
    match table[(code & 0x7F) as usize] {
        0 => None,
        usage => Some(Key::from_hid(usage)),
    }
}

/// The scan code and E0 flag for `key`; the inverse of [`to_key`].
pub(super) fn from_key(key: Key) -> Option<(u8, bool)> {
    // Pause has no code of its own (see PLAIN). Extended first, so Print Screen and Num Lock get
    // their primary codes rather than the Alt+Print Screen and Pause aliases.
    if key == Key::PAUSE {
        return Some((0x45, false));
    }
    let find = |table: &[u8; 128]| table.iter().position(|&u| u != 0 && u == key.hid());
    if let Some(code) = find(&EXTENDED) {
        return Some((code as u8, true));
    }
    find(&PLAIN).map(|code| (code as u8, false))
}
