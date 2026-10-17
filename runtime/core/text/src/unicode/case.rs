//! Case folding, caseless comparison and simple case mapping.
//!
//! Folding maps the characters of a string so that strings differing only in case become equal
//! (Unicode §3.13). *Full* folding ([`fold`], CaseFolding.txt statuses C and F) may turn one
//! character into several (`ß` → `ss`, `ŉ` → `ʼn`) and is what caseless matching uses. *Simple*
//! folding ([`simple_fold`], statuses C and S) keeps one character per character, for when
//! lengths must not change. Neither applies the Turkic entries (status T).
//!
//! [`to_lower`] / [`to_upper`] are the simple mappings of UnicodeData.txt: one character to
//! one, no context and no language. There is no SpecialCasing.txt data, so `ß` uppercases to
//! itself and `Σ` always lowercases to `σ`, even at the end of a word. Use folding, not
//! lowercasing, to compare.

use alloc::string::String;
use core::cmp::Ordering;
use core::hash::{Hash, Hasher};
use core::str::Chars;

use super::normalize::Decompositions;
use super::tables::*;
use super::{StrMap, delta_map};

const FOLD_FULL: StrMap = StrMap {
    keys: FOLD_FULL_KEYS,
    index: FOLD_FULL_INDEX,
    offsets: FOLD_FULL_OFFSETS,
    pool: FOLD_FULL_POOL,
};

/// The simple lowercase mapping of `c` (`c` itself if it has none).
#[inline]
pub fn to_lower(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    delta_map(LOWER_KEYS, LOWER_DELTAS, c).unwrap_or(c)
}

/// The simple uppercase mapping of `c` (`c` itself if it has none).
#[inline]
pub fn to_upper(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_uppercase();
    }
    delta_map(UPPER_KEYS, UPPER_DELTAS, c).unwrap_or(c)
}

/// `s` with every character simply lowercased ([`to_lower`]).
pub fn to_lowercase(s: &str) -> String {
    s.chars().map(to_lower).collect()
}

/// `s` with every character simply uppercased ([`to_upper`]).
pub fn to_uppercase(s: &str) -> String {
    s.chars().map(to_upper).collect()
}

/// The simple case folding of `c`: one character (CaseFolding.txt statuses C and S).
#[inline]
pub fn simple_fold(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    delta_map(FOLD_KEYS, FOLD_DELTAS, c).unwrap_or(c)
}

/// The simple case folding of `s`, character by character.
pub fn fold_simple(s: &str) -> core::iter::Map<Chars<'_>, fn(char) -> char> {
    s.chars().map(simple_fold as fn(char) -> char)
}

/// The full case folding of `s` (statuses C and F), as an iterator of characters.
pub fn fold(s: &str) -> CaseFold<Chars<'_>> {
    CaseFold::new(s.chars())
}

/// Full case folding over any `char` iterator: see [`fold`].
#[derive(Clone)]
pub struct CaseFold<I> {
    chars: I,
    /// The rest of a multi-character folding.
    pending: Chars<'static>,
}

impl<I: Iterator<Item = char>> CaseFold<I> {
    pub fn new(chars: I) -> Self {
        Self {
            chars,
            pending: "".chars(),
        }
    }
}

impl<I: Iterator<Item = char>> Iterator for CaseFold<I> {
    type Item = char;

    #[inline]
    fn next(&mut self) -> Option<char> {
        if let Some(c) = self.pending.next() {
            return Some(c);
        }
        let c = self.chars.next()?;
        if c.is_ascii() {
            return Some(c.to_ascii_lowercase());
        }
        if let Some(full) = FOLD_FULL.get(c as u32) {
            self.pending = full.chars();
            return self.pending.next();
        }
        Some(delta_map(FOLD_KEYS, FOLD_DELTAS, c).unwrap_or(c))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let (lo, _) = self.chars.size_hint();
        (lo + self.pending.as_str().len().min(1), None)
    }
}

/// Whether `a` and `b` are equal ignoring case: default caseless matching (Unicode D144), their
/// full case foldings compared. Does not normalize; see [`eq_ignore_case_canonical`].
pub fn eq_ignore_case(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    fold(a).eq(fold(b))
}

/// Canonical caseless matching (Unicode D145): `NFD(fold(NFD(a))) == NFD(fold(NFD(b)))`, so
/// precomposed and decomposed spellings (`é` and `e` + U+0301) match too.
pub fn eq_ignore_case_canonical(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    canonical_fold(a).eq(canonical_fold(b))
}

fn canonical_fold(s: &str) -> Decompositions<CaseFold<Decompositions<Chars<'_>>>> {
    Decompositions::new(CaseFold::new(Decompositions::new(s.chars(), false)), false)
}

/// Orders `a` and `b` by code point after full case folding: consistent with
/// [`eq_ignore_case`], but not a linguistic (collation) order.
pub fn cmp_ignore_case(a: &str, b: &str) -> Ordering {
    fold(a).cmp(fold(b))
}

/// A `&str` that compares, orders and hashes by its full case folding: a map key that ignores
/// case in every script. The Unicode-aware counterpart of
/// [`utf8::CaseInsensitive`](crate::utf8::CaseInsensitive).
#[derive(Clone, Copy, Debug)]
pub struct Caseless<'a>(pub &'a str);

impl PartialEq for Caseless<'_> {
    fn eq(&self, other: &Self) -> bool {
        eq_ignore_case(self.0, other.0)
    }
}

impl Eq for Caseless<'_> {}

impl PartialOrd for Caseless<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Caseless<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        cmp_ignore_case(self.0, other.0)
    }
}

impl Hash for Caseless<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let mut buf = [0u8; 4];
        for c in fold(self.0) {
            state.write(c.encode_utf8(&mut buf).as_bytes());
        }
        state.write_u8(0xff);
    }
}
