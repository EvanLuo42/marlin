//! UTF-8 and ASCII-case helpers that `core::str` lacks.
//!
//! Case-insensitivity here is ASCII only (`A-Z` vs `a-z`), like `str::eq_ignore_ascii_case`:
//! engine identifiers, paths and config keys are ASCII, and full Unicode case folding needs
//! tables this layer should not carry.

use core::cmp::Ordering;
use core::hash::{Hash, Hasher};

use marlin_hash::xxh3_64_ascii_lowercase;

/// Compares as if both strings were ASCII-lowercased.
pub fn cmp_ignore_ascii_case(a: &str, b: &str) -> Ordering {
    let lower = |c: &u8| c.to_ascii_lowercase();
    a.as_bytes()
        .iter()
        .map(lower)
        .cmp(b.as_bytes().iter().map(lower))
}

pub fn starts_with_ignore_ascii_case(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

pub fn ends_with_ignore_ascii_case(s: &str, suffix: &str) -> bool {
    s.len() >= suffix.len()
        && s.as_bytes()[s.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
}

/// Byte offset of the first ASCII-case-insensitive occurrence of `needle`. Always a char
/// boundary: a match starts with the same ASCII letter (either case) or the same lead byte.
pub fn find_ignore_ascii_case(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

pub fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    find_ignore_ascii_case(haystack, needle).is_some()
}

#[inline]
const fn is_boundary_byte(b: u8) -> bool {
    // Anything but a continuation byte (0b10xx_xxxx) starts a char.
    (b as i8) >= -0x40
}

/// The largest char boundary `<= index` (clamped to `s.len()`).
pub const fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let b = s.as_bytes();
    let mut i = index;
    while !is_boundary_byte(b[i]) {
        i -= 1;
    }
    i
}

/// The smallest char boundary `>= index` (clamped to `s.len()`).
pub const fn ceil_char_boundary(s: &str, index: usize) -> usize {
    let b = s.as_bytes();
    let mut i = index;
    while i < b.len() && !is_boundary_byte(b[i]) {
        i += 1;
    }
    if i > b.len() { b.len() } else { i }
}

/// The longest prefix of `s` that is at most `max_len` bytes and ends on a char boundary.
pub fn truncate_to(s: &str, max_len: usize) -> &str {
    &s[..floor_char_boundary(s, max_len)]
}

/// Length of the UTF-8 sequence that starts with `first`, or 0 if `first` cannot start one.
pub const fn char_width(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 0,
    }
}

/// A `&str` that compares, orders and hashes ignoring ASCII case: a map key for config names,
/// file extensions and the like.
#[derive(Clone, Copy, Debug)]
pub struct CaseInsensitive<'a>(pub &'a str);

impl PartialEq for CaseInsensitive<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(other.0)
    }
}

impl Eq for CaseInsensitive<'_> {}

impl PartialOrd for CaseInsensitive<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CaseInsensitive<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        cmp_ignore_ascii_case(self.0, other.0)
    }
}

impl Hash for CaseInsensitive<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(xxh3_64_ascii_lowercase(self.0.as_bytes()));
    }
}
