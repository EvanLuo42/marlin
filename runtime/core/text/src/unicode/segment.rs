//! Text segmentation (UAX #29): extended grapheme clusters and word boundaries.
//!
//! [`graphemes`] splits a string into what users perceive as characters: `e` + U+0301, a flag
//! (two regional indicators), a family emoji joined by ZWJs, a Hangul syllable spelled in jamo.
//! It implements the default extended grapheme cluster rules GB1–GB13 and GB999 except GB9c
//! (see the [module docs](super)).
//!
//! [`word_bounds`] splits at every default word boundary (WB1–WB999), returning the words and
//! everything between them (spaces, punctuation) as separate pieces. Ideographs are one piece
//! each: dictionary-based segmentation of Chinese, Japanese and Thai is out of scope.
//!
//! ```
//! use marlin_text::unicode::{graphemes, word_bounds};
//!
//! assert!(graphemes("ne\u{301}e 🇫🇷").eq(["n", "e\u{301}", "e", " ", "🇫🇷"]));
//! assert!(word_bounds("Can't stop, 3.14!").eq(["Can't", " ", "stop", ",", " ", "3.14", "!"]));
//! ```

use super::range_value;
use super::tables::*;

const NONE: u8 = u8::MAX;

#[inline]
fn grapheme_property(c: char) -> u8 {
    let cp = c as u32;
    if (0x20..0x7f).contains(&cp) {
        return GCB_OTHER;
    }
    let s = cp.wrapping_sub(0xac00);
    if s < 11172 {
        return if s % 28 == 0 { GCB_LV } else { GCB_LVT };
    }
    range_value(GRAPHEME_KEYS, GRAPHEME_VALUES, cp)
}

#[inline]
fn is_ext_pict(c: char) -> bool {
    (c as u32) >= 0xa9 && grapheme_property(c) & EXT_PICT != 0
}

#[inline]
fn word_property(c: char) -> u8 {
    if c.is_ascii_alphabetic() {
        return WB_ALETTER;
    }
    range_value(WORD_KEYS, WORD_VALUES, c as u32)
}

/// Splits `s` into extended grapheme clusters.
pub fn graphemes(s: &str) -> Graphemes<'_> {
    Graphemes { rest: s }
}

/// The extended grapheme clusters of `s` with their byte offsets.
pub fn grapheme_indices(s: &str) -> GraphemeIndices<'_> {
    GraphemeIndices {
        base: s.as_ptr().addr(),
        inner: graphemes(s),
    }
}

/// The iterator of [`graphemes`].
#[derive(Clone, Debug)]
pub struct Graphemes<'a> {
    rest: &'a str,
}

impl<'a> Graphemes<'a> {
    /// The text not yet returned.
    pub fn as_str(&self) -> &'a str {
        self.rest
    }
}

/// GB11's progress: `ExtPict Extend* ZWJ × ExtPict`.
#[derive(Clone, Copy, PartialEq)]
enum Pictographic {
    None,
    /// After an Extended_Pictographic and any Extend.
    Seen,
    /// After that sequence and a ZWJ.
    Joined,
}

impl<'a> Iterator for Graphemes<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        let mut chars = self.rest.char_indices();
        let (_, first) = chars.next()?;
        let mut prev = grapheme_property(first);
        let mut pict = if prev & EXT_PICT != 0 {
            Pictographic::Seen
        } else {
            Pictographic::None
        };
        prev &= !EXT_PICT;
        // Regional indicators in a row ending at `prev`.
        let mut regional = u32::from(prev == GCB_REGIONAL_INDICATOR);
        let mut end = self.rest.len();
        for (i, c) in chars {
            let raw = grapheme_property(c);
            let cur = raw & !EXT_PICT;
            let joins = match (prev, cur) {
                (GCB_CR, GCB_LF) => true,                                            // GB3
                (GCB_CONTROL | GCB_CR | GCB_LF, _) => false,                         // GB4
                (_, GCB_CONTROL | GCB_CR | GCB_LF) => false,                         // GB5
                (GCB_L, GCB_L | GCB_V | GCB_LV | GCB_LVT) => true,                   // GB6
                (GCB_LV | GCB_V, GCB_V | GCB_T) => true,                             // GB7
                (GCB_LVT | GCB_T, GCB_T) => true,                                    // GB8
                (_, GCB_EXTEND | GCB_ZWJ | GCB_SPACINGMARK) => true,                 // GB9, GB9a
                (GCB_PREPEND, _) => true,                                            // GB9b
                (GCB_ZWJ, _) if raw & EXT_PICT != 0 => pict == Pictographic::Joined, // GB11
                (GCB_REGIONAL_INDICATOR, GCB_REGIONAL_INDICATOR) => regional % 2 == 1, // GB12, GB13
                _ => false,                                                          // GB999
            };
            if !joins {
                end = i;
                break;
            }
            pict = match (cur, pict) {
                _ if raw & EXT_PICT != 0 => Pictographic::Seen,
                (GCB_EXTEND, Pictographic::Seen) => Pictographic::Seen,
                (GCB_ZWJ, Pictographic::Seen) => Pictographic::Joined,
                _ => Pictographic::None,
            };
            regional = if cur == GCB_REGIONAL_INDICATOR {
                regional + 1
            } else {
                0
            };
            prev = cur;
        }
        let (cluster, rest) = self.rest.split_at(end);
        self.rest = rest;
        Some(cluster)
    }
}

/// The iterator of [`grapheme_indices`].
#[derive(Clone, Debug)]
pub struct GraphemeIndices<'a> {
    base: usize,
    inner: Graphemes<'a>,
}

impl<'a> Iterator for GraphemeIndices<'a> {
    type Item = (usize, &'a str);

    fn next(&mut self) -> Option<(usize, &'a str)> {
        let s = self.inner.next()?;
        Some((s.as_ptr().addr() - self.base, s))
    }
}

/// Splits `s` at every word boundary.
pub fn word_bounds(s: &str) -> WordBounds<'_> {
    WordBounds {
        rest: s,
        state: WordState::START,
    }
}

/// The pieces of [`word_bounds`] with their byte offsets.
pub fn word_bound_indices(s: &str) -> WordBoundIndices<'_> {
    WordBoundIndices {
        base: s.as_ptr().addr(),
        inner: word_bounds(s),
    }
}

/// The iterator of [`word_bounds`].
#[derive(Clone, Debug)]
pub struct WordBounds<'a> {
    rest: &'a str,
    state: WordState,
}

impl<'a> WordBounds<'a> {
    /// The text not yet returned.
    pub fn as_str(&self) -> &'a str {
        self.rest
    }
}

/// What the word rules look back at. Properties are `WB_*`, or `NONE` at the start.
#[derive(Clone, Copy, Debug)]
struct WordState {
    /// The previous character itself (WB3–WB3d look at it).
    actual: u8,
    /// The previous character that WB4 did not fold into the one before it.
    prev: u8,
    /// The one before `prev`, likewise.
    before: u8,
    /// Regional indicators in a row (ignoring WB4) ending at `prev`.
    regional: u32,
}

impl WordState {
    const START: Self = Self {
        actual: NONE,
        prev: NONE,
        before: NONE,
        regional: 0,
    };

    fn advance(&mut self, cur: u8) {
        // WB4: Extend, Format and ZWJ attach to what precedes them, unless that is the start
        // of text or a line break.
        let attaches =
            is_ignorable(cur) && !matches!(self.actual, NONE | WB_CR | WB_LF | WB_NEWLINE);
        self.actual = cur;
        if attaches {
            return;
        }
        self.regional = if cur == WB_REGIONAL_INDICATOR {
            self.regional + 1
        } else {
            0
        };
        self.before = self.prev;
        self.prev = cur;
    }

    /// Whether there is a boundary between the text so far and `c`; `after` is the text
    /// following `c`.
    fn breaks(&self, c: char, cur: u8, after: &str) -> bool {
        let (actual, prev, before) = (self.actual, self.prev, self.before);
        if actual == WB_CR && cur == WB_LF {
            return false; // WB3
        }
        if matches!(actual, WB_NEWLINE | WB_CR | WB_LF) || matches!(cur, WB_NEWLINE | WB_CR | WB_LF)
        {
            return true; // WB3a, WB3b
        }
        if actual == WB_ZWJ && is_ext_pict(c) {
            return false; // WB3c
        }
        if actual == WB_WSEGSPACE && cur == WB_WSEGSPACE {
            return false; // WB3d
        }
        if is_ignorable(cur) {
            return false; // WB4
        }
        // The next character that WB4 does not fold away, for the rules that look ahead.
        let next = || {
            after
                .chars()
                .map(word_property)
                .find(|&p| !is_ignorable(p))
                .unwrap_or(NONE)
        };
        // Every remaining rule but WB999 forbids a break.
        let joins = (is_ahletter(prev) && is_ahletter(cur)) // WB5
            || (is_ahletter(prev) && is_mid_letter(cur) && is_ahletter(next())) // WB6
            || (is_ahletter(before) && is_mid_letter(prev) && is_ahletter(cur)) // WB7
            || (prev == WB_HEBREW_LETTER && cur == WB_SINGLE_QUOTE) // WB7a
            || (prev == WB_HEBREW_LETTER && cur == WB_DOUBLE_QUOTE && next() == WB_HEBREW_LETTER) // WB7b
            || (before == WB_HEBREW_LETTER && prev == WB_DOUBLE_QUOTE && cur == WB_HEBREW_LETTER) // WB7c
            || (prev == WB_NUMERIC && cur == WB_NUMERIC) // WB8
            || (is_ahletter(prev) && cur == WB_NUMERIC) // WB9
            || (prev == WB_NUMERIC && is_ahletter(cur)) // WB10
            || (before == WB_NUMERIC && is_mid_num(prev) && cur == WB_NUMERIC) // WB11
            || (prev == WB_NUMERIC && is_mid_num(cur) && next() == WB_NUMERIC) // WB12
            || (prev == WB_KATAKANA && cur == WB_KATAKANA) // WB13
            || (cur == WB_EXTENDNUMLET
                && (is_ahletter(prev) || matches!(prev, WB_NUMERIC | WB_KATAKANA | WB_EXTENDNUMLET))) // WB13a
            || (prev == WB_EXTENDNUMLET
                && (is_ahletter(cur) || matches!(cur, WB_NUMERIC | WB_KATAKANA))) // WB13b
            || (prev == WB_REGIONAL_INDICATOR
                && cur == WB_REGIONAL_INDICATOR
                && self.regional % 2 == 1); // WB15, WB16
        !joins
    }
}

#[inline]
fn is_ignorable(p: u8) -> bool {
    matches!(p, WB_EXTEND | WB_FORMAT | WB_ZWJ)
}

#[inline]
fn is_ahletter(p: u8) -> bool {
    matches!(p, WB_ALETTER | WB_HEBREW_LETTER)
}

/// MidLetter or MidNumLetQ.
#[inline]
fn is_mid_letter(p: u8) -> bool {
    matches!(p, WB_MIDLETTER | WB_MIDNUMLET | WB_SINGLE_QUOTE)
}

/// MidNum or MidNumLetQ.
#[inline]
fn is_mid_num(p: u8) -> bool {
    matches!(p, WB_MIDNUM | WB_MIDNUMLET | WB_SINGLE_QUOTE)
}

impl<'a> Iterator for WordBounds<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        let mut chars = self.rest.char_indices();
        let (_, first) = chars.next()?;
        self.state.advance(word_property(first));
        let mut end = self.rest.len();
        for (i, c) in chars {
            let cur = word_property(c);
            if self.state.breaks(c, cur, &self.rest[i + c.len_utf8()..]) {
                end = i;
                break;
            }
            self.state.advance(cur);
        }
        let (piece, rest) = self.rest.split_at(end);
        self.rest = rest;
        Some(piece)
    }
}

/// The iterator of [`word_bound_indices`].
#[derive(Clone, Debug)]
pub struct WordBoundIndices<'a> {
    base: usize,
    inner: WordBounds<'a>,
}

impl<'a> Iterator for WordBoundIndices<'a> {
    type Item = (usize, &'a str);

    fn next(&mut self) -> Option<(usize, &'a str)> {
        let s = self.inner.next()?;
        Some((s.as_ptr().addr() - self.base, s))
    }
}
