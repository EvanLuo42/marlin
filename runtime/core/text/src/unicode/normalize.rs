//! Normalization forms NFC, NFD, NFKC and NFKD (UAX #15).
//!
//! [`Decompositions`] maps each character to its full canonical (or compatibility)
//! decomposition, Hangul syllables algorithmically, and puts each run of combining marks in
//! canonical order (stable sort by Canonical_Combining_Class). [`Recompositions`] then
//! recombines with the canonical composition algorithm: primary composites only, the
//! Full_Composition_Exclusion characters never, Hangul algorithmically.
//!
//! Both are iterators over any `char` iterator; they buffer one starter and the marks after it,
//! inline up to 16 characters. [`quick_check`] answers most [`is_normalized`] questions without
//! normalizing.
//!
//! ```
//! use marlin_text::unicode::{Form, is_nfc, nfc, nfd, normalize};
//!
//! assert!(nfd("é").eq(['e', '\u{301}']));
//! assert!(nfc("e\u{301}").eq(['é']));
//! assert_eq!(normalize("ﬁ", Form::Nfkc), "fi");
//! assert!(is_nfc("é") && !is_nfc("e\u{301}"));
//! ```

use alloc::string::String;
use core::fmt;
use core::str::Chars;

use marlin_containers::SmallVec;

use super::tables::*;
use super::{StrMap, range_value};

const CANONICAL: StrMap = StrMap {
    keys: CANONICAL_KEYS,
    index: CANONICAL_INDEX,
    offsets: CANONICAL_OFFSETS,
    pool: CANONICAL_POOL,
};
const COMPAT: StrMap = StrMap {
    keys: COMPAT_KEYS,
    index: COMPAT_INDEX,
    offsets: COMPAT_OFFSETS,
    pool: COMPAT_POOL,
};

const S_BASE: u32 = 0xac00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11a7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;

/// Marks sorted by insertion sort up to this run length, by the (allocating) stable sort above.
const INSERTION_SORT_MAX: usize = 32;

/// A Unicode normalization form.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Form {
    /// Canonical decomposition, then canonical composition.
    Nfc,
    /// Canonical decomposition.
    Nfd,
    /// Compatibility decomposition, then canonical composition.
    Nfkc,
    /// Compatibility decomposition.
    Nfkd,
}

impl Form {
    #[inline]
    const fn compat(self) -> bool {
        matches!(self, Self::Nfkc | Self::Nfkd)
    }

    #[inline]
    const fn composes(self) -> bool {
        matches!(self, Self::Nfc | Self::Nfkc)
    }
}

/// The answer of [`quick_check`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IsNormalized {
    Yes,
    No,
    /// Only normalizing can tell (NFC and NFKC only).
    Maybe,
}

/// The Canonical_Combining_Class of `c` (0 for starters).
#[inline]
pub fn combining_class(c: char) -> u8 {
    if (c as u32) < 0x300 {
        0
    } else {
        range_value(CCC_KEYS, CCC_VALUES, c as u32)
    }
}

#[inline]
fn is_hangul_syllable(cp: u32) -> bool {
    cp.wrapping_sub(S_BASE) < S_COUNT
}

/// The canonical composition of the pair `a` `b`, if it is a primary composite.
fn compose(a: char, b: char) -> Option<char> {
    let (a, b) = (a as u32, b as u32);
    if a.wrapping_sub(L_BASE) < L_COUNT && b.wrapping_sub(V_BASE) < V_COUNT {
        let lv = S_BASE + ((a - L_BASE) * V_COUNT + (b - V_BASE)) * T_COUNT;
        return char::from_u32(lv);
    }
    if is_hangul_syllable(a)
        && (a - S_BASE).is_multiple_of(T_COUNT)
        && b > T_BASE
        && b < T_BASE + T_COUNT
    {
        return char::from_u32(a + (b - T_BASE));
    }
    let key = u64::from(a) << 21 | u64::from(b);
    let i = COMPOSE.partition_point(|&e| e >> 21 < key);
    match COMPOSE.get(i) {
        Some(&e) if e >> 21 == key => char::from_u32((e & 0x1f_ffff) as u32),
        _ => None,
    }
}

/// Characters in canonical (or compatibility) decomposition and canonical order: NFD or NFKD.
#[derive(Clone)]
pub struct Decompositions<I> {
    chars: I,
    compat: bool,
    /// Decomposed characters with their combining class. `buffer[..ready]` is in canonical
    /// order and final; the rest waits for the next starter.
    buffer: SmallVec<(char, u8), 16>,
    ready: usize,
    /// Next index of `buffer[..ready]` to return.
    next: usize,
}

impl<I: Iterator<Item = char>> Decompositions<I> {
    /// Decomposes `chars`: canonically (NFD), or with `compat` also by compatibility (NFKD).
    pub fn new(chars: I, compat: bool) -> Self {
        Self {
            chars,
            compat,
            buffer: SmallVec::new(),
            ready: 0,
            next: 0,
        }
    }

    #[inline]
    fn push(&mut self, c: char) {
        let class = combining_class(c);
        if class == 0 {
            sort_marks(&mut self.buffer[self.ready..]);
            self.ready = self.buffer.len();
        }
        self.buffer.push((c, class));
    }

    fn push_decomposed(&mut self, c: char) {
        let cp = c as u32;
        if cp < 0xa0 {
            return self.push(c);
        }
        if is_hangul_syllable(cp) {
            let s = cp - S_BASE;
            for part in [
                L_BASE + s / N_COUNT,
                V_BASE + s % N_COUNT / T_COUNT,
                T_BASE + s % T_COUNT,
            ] {
                // The jamo are scalar values; T_BASE itself means "no trailing consonant".
                if part != T_BASE
                    && let Some(jamo) = char::from_u32(part)
                {
                    self.push(jamo);
                }
            }
            return;
        }
        let mapping = if self.compat {
            COMPAT.get(cp).or_else(|| CANONICAL.get(cp))
        } else {
            CANONICAL.get(cp)
        };
        match mapping {
            Some(m) => m.chars().for_each(|d| self.push(d)),
            None => self.push(c),
        }
    }
}

/// Stable sort by combining class.
fn sort_marks(marks: &mut [(char, u8)]) {
    if marks.len() > INSERTION_SORT_MAX {
        marks.sort_by_key(|&(_, class)| class);
        return;
    }
    for i in 1..marks.len() {
        let mut j = i;
        while j > 0 && marks[j - 1].1 > marks[j].1 {
            marks.swap(j - 1, j);
            j -= 1;
        }
    }
}

impl<I: Iterator<Item = char>> Iterator for Decompositions<I> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        loop {
            if self.next < self.ready {
                self.next += 1;
                return Some(self.buffer[self.next - 1].0);
            }
            if self.ready > 0 {
                let len = self.buffer.len();
                self.buffer.copy_within(self.ready.., 0);
                self.buffer.truncate(len - self.ready);
                self.ready = 0;
                self.next = 0;
            }
            match self.chars.next() {
                Some(c) => self.push_decomposed(c),
                None if self.buffer.is_empty() => return None,
                None => {
                    sort_marks(&mut self.buffer);
                    self.ready = self.buffer.len();
                }
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let (lo, _) = self.chars.size_hint();
        (lo + (self.buffer.len() - self.next), None)
    }
}

/// [`Decompositions`] canonically recomposed: NFC or NFKC.
#[derive(Clone)]
pub struct Recompositions<I> {
    chars: Decompositions<I>,
    /// The last starter (if `has_starter`) and the characters after it that did not combine.
    window: SmallVec<char, 16>,
    has_starter: bool,
    /// Combining class of the last character in `window` after the starter; `None` if the
    /// starter is the last.
    last_class: Option<u8>,
    /// Finished output, returned from `out[next..]`.
    out: SmallVec<char, 16>,
    next: usize,
    done: bool,
}

impl<I: Iterator<Item = char>> Recompositions<I> {
    /// Normalizes `chars` to NFC, or with `compat` to NFKC.
    pub fn new(chars: I, compat: bool) -> Self {
        Self {
            chars: Decompositions::new(chars, compat),
            window: SmallVec::new(),
            has_starter: false,
            last_class: None,
            out: SmallVec::new(),
            next: 0,
            done: false,
        }
    }

    fn feed(&mut self, c: char) {
        let class = combining_class(c);
        if self.has_starter {
            // `c` is blocked from the starter by a character in between of class 0 or >= its own.
            let blocked = self.last_class.is_some_and(|last| last >= class);
            if !blocked && let Some(composite) = compose(self.window[0], c) {
                self.window[0] = composite;
                return;
            }
        }
        if class == 0 {
            self.out.extend_from_slice(&self.window);
            self.window.clear();
            self.window.push(c);
            self.has_starter = true;
            self.last_class = None;
        } else if self.has_starter {
            self.window.push(c);
            self.last_class = Some(class);
        } else {
            // Marks before the first starter have nothing to combine with.
            self.out.push(c);
        }
    }
}

impl<I: Iterator<Item = char>> Iterator for Recompositions<I> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        loop {
            if self.next < self.out.len() {
                self.next += 1;
                return Some(self.out[self.next - 1]);
            }
            self.out.clear();
            self.next = 0;
            if self.done {
                return None;
            }
            match self.chars.next() {
                Some(c) => self.feed(c),
                None => {
                    self.done = true;
                    self.out.extend_from_slice(&self.window);
                    self.window.clear();
                }
            }
        }
    }
}

/// `s` in NFD, as characters.
pub fn nfd(s: &str) -> Decompositions<Chars<'_>> {
    Decompositions::new(s.chars(), false)
}

/// `s` in NFKD, as characters.
pub fn nfkd(s: &str) -> Decompositions<Chars<'_>> {
    Decompositions::new(s.chars(), true)
}

/// `s` in NFC, as characters.
pub fn nfc(s: &str) -> Recompositions<Chars<'_>> {
    Recompositions::new(s.chars(), false)
}

/// `s` in NFKC, as characters.
pub fn nfkc(s: &str) -> Recompositions<Chars<'_>> {
    Recompositions::new(s.chars(), true)
}

/// Writes `s` in `form` to `out`.
pub fn normalize_to(s: &str, form: Form, out: &mut impl fmt::Write) -> fmt::Result {
    if form.composes() {
        Recompositions::new(s.chars(), form.compat()).try_for_each(|c| out.write_char(c))
    } else {
        Decompositions::new(s.chars(), form.compat()).try_for_each(|c| out.write_char(c))
    }
}

/// `s` in `form`. Copies `s` unchanged when the quick check says it is already normalized.
pub fn normalize(s: &str, form: Form) -> String {
    if quick_check(s, form) == IsNormalized::Yes {
        return s.into();
    }
    let mut out = String::with_capacity(s.len());
    // Writing to a `String` cannot fail.
    let _ = normalize_to(s, form, &mut out);
    out
}

/// Whether `s` is in `form`, as far as the UAX #15 quick check can tell without normalizing.
pub fn quick_check(s: &str, form: Form) -> IsNormalized {
    let mut last_class = 0;
    let mut result = IsNormalized::Yes;
    for c in s.chars() {
        let cp = c as u32;
        if cp < 0xa0 {
            last_class = 0;
            continue;
        }
        let class = combining_class(c);
        if last_class > class && class != 0 {
            return IsNormalized::No;
        }
        last_class = class;
        let check = match form {
            Form::Nfc => range_value(QC_KEYS, QC_VALUES, cp) & 3,
            Form::Nfkc => range_value(QC_KEYS, QC_VALUES, cp) >> 2 & 3,
            Form::Nfd => u8::from(is_hangul_syllable(cp) || CANONICAL.get(cp).is_some()),
            Form::Nfkd => u8::from(
                is_hangul_syllable(cp) || COMPAT.get(cp).is_some() || CANONICAL.get(cp).is_some(),
            ),
        };
        match check {
            0 => {}
            1 => return IsNormalized::No,
            _ => result = IsNormalized::Maybe,
        }
    }
    result
}

/// Whether `s` is in `form`. Normalizes (without allocating) only when the quick check cannot
/// tell.
pub fn is_normalized(s: &str, form: Form) -> bool {
    match quick_check(s, form) {
        IsNormalized::Yes => true,
        IsNormalized::No => false,
        IsNormalized::Maybe if form.composes() => {
            s.chars().eq(Recompositions::new(s.chars(), form.compat()))
        }
        IsNormalized::Maybe => s.chars().eq(Decompositions::new(s.chars(), form.compat())),
    }
}

pub fn is_nfc(s: &str) -> bool {
    is_normalized(s, Form::Nfc)
}

pub fn is_nfd(s: &str) -> bool {
    is_normalized(s, Form::Nfd)
}

pub fn is_nfkc(s: &str) -> bool {
    is_normalized(s, Form::Nfkc)
}

pub fn is_nfkd(s: &str) -> bool {
    is_normalized(s, Form::Nfkd)
}
