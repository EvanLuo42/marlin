//! CLDR cardinal plural rules.
//!
//! The rules are hand-written from CLDR's `supplemental/plurals.xml` (the cardinal rules, as of
//! CLDR 42 and unchanged since; each function names its CLDR locale group and quotes its rule).
//! Operands follow UTS #35 (Part 3, "Plural Operand Meanings"): `n` absolute value, `i`
//! integer digits, `v` / `w` count of visible fraction digits with / without trailing zeros,
//! `f` / `t` those digits as an integer. The compact exponent operands `c` / `e` are always 0:
//! numbers here are never written as `1.2c6`.

use core::fmt;

use super::Locale;

/// A CLDR plural category.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum PluralCategory {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

impl PluralCategory {
    pub const ALL: [Self; 6] = [
        Self::Zero,
        Self::One,
        Self::Two,
        Self::Few,
        Self::Many,
        Self::Other,
    ];

    /// The CLDR keyword: `"zero"`, `"one"`, … `"other"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Zero => "zero",
            Self::One => "one",
            Self::Two => "two",
            Self::Few => "few",
            Self::Many => "many",
            Self::Other => "other",
        }
    }

    /// The category named by a CLDR keyword.
    pub fn from_keyword(keyword: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == keyword)
    }
}

impl fmt::Display for PluralCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The operands plural rules look at: a number as written, so `1` and `1.0` differ.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct PluralOperands {
    /// Integer digits of the absolute value.
    pub i: u64,
    /// Number of visible fraction digits, with trailing zeros.
    pub v: u32,
    /// Number of visible fraction digits, without trailing zeros.
    pub w: u32,
    /// Visible fraction digits, with trailing zeros, as an integer.
    pub f: u64,
    /// Visible fraction digits, without trailing zeros, as an integer.
    pub t: u64,
}

impl PluralOperands {
    /// The operands of an integer.
    pub const fn from_int(n: i64) -> Self {
        Self {
            i: n.unsigned_abs(),
            v: 0,
            w: 0,
            f: 0,
            t: 0,
        }
    }

    /// The operands of a decimal number written as ASCII `[-]digits[.digits]` (`"1.50"`); `None`
    /// if `s` is not one, or has more than 18 fraction digits.
    pub fn from_decimal(s: &str) -> Option<Self> {
        let s = s.strip_prefix('-').unwrap_or(s);
        let (int, frac) = s.split_once('.').unwrap_or((s, ""));
        let digits = |d: &str| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit());
        if !digits(int) || (s.contains('.') && !digits(frac)) || frac.len() > 18 {
            return None;
        }
        // Integer digits beyond u64 only matter modulo powers of ten: keep the last 18.
        let int = &int[int.len().saturating_sub(18)..];
        let trimmed = frac.trim_end_matches('0');
        let value = |d: &str| d.bytes().fold(0u64, |n, b| n * 10 + u64::from(b - b'0'));
        Some(Self {
            i: value(int),
            v: frac.len() as u32,
            w: trimmed.len() as u32,
            f: value(frac),
            t: value(trimmed),
        })
    }

    /// Whether the number is exactly the integer `n` (CLDR's `n = <n>`).
    #[inline]
    const fn is(&self, n: u64) -> bool {
        self.i == n && self.f == 0
    }

    #[inline]
    const fn is_integer(&self) -> bool {
        self.f == 0
    }

    /// CLDR's "many" for numbers in the millions in es, fr, it, pt and pt-PT:
    /// `e = 0 and i != 0 and i % 1000000 = 0 and v = 0 or e != 0..5`.
    #[inline]
    const fn is_whole_millions(&self) -> bool {
        self.v == 0 && self.i != 0 && self.i.is_multiple_of(1_000_000)
    }
}

impl From<i64> for PluralOperands {
    fn from(n: i64) -> Self {
        Self::from_int(n)
    }
}

/// The cardinal plural rules of one language.
#[derive(Clone, Copy)]
pub struct PluralRules {
    select: fn(&PluralOperands) -> PluralCategory,
    categories: &'static [PluralCategory],
}

impl PluralRules {
    /// The rules for `locale`'s language (and region, for `pt-PT`). Languages without rules
    /// here get CLDR root's: everything is `other`.
    pub fn for_locale(locale: &Locale) -> Self {
        use PluralCategory::*;
        const ONE: &[PluralCategory] = &[One, Other];
        const ONE_MANY: &[PluralCategory] = &[One, Many, Other];
        const SLAVIC: &[PluralCategory] = &[One, Few, Many, Other];
        let (select, categories): (fn(&PluralOperands) -> PluralCategory, _) =
            match (locale.language(), locale.region()) {
                ("pt", Some("PT")) => (catalan_italian, ONE_MANY),
                ("en" | "de" | "nl" | "sv" | "fi", _) => (one_if_integer_one, ONE),
                ("da", _) => (danish, ONE),
                ("nb" | "no" | "nn" | "tr", _) => (one_if_one, ONE),
                ("es", _) => (spanish, ONE_MANY),
                ("it" | "ca", _) => (catalan_italian, ONE_MANY),
                ("fr", _) => (french, ONE_MANY),
                ("pt", _) => (portuguese, ONE_MANY),
                ("ru" | "uk", _) => (russian, SLAVIC),
                ("pl", _) => (polish, SLAVIC),
                ("cs" | "sk", _) => (czech, SLAVIC),
                ("ar", _) => (arabic, &[Zero, One, Two, Few, Many, Other]),
                ("he", _) => (hebrew, &[One, Two, Other]),
                ("hi", _) => (hindi, ONE),
                _ => (other, &[Other]),
            };
        Self { select, categories }
    }

    pub fn select(&self, operands: &PluralOperands) -> PluralCategory {
        (self.select)(operands)
    }

    /// Every category this language uses, in [`PluralCategory`] order; always ends in `Other`.
    pub fn categories(&self) -> &'static [PluralCategory] {
        self.categories
    }
}

impl fmt::Debug for PluralRules {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluralRules")
            .field("categories", &self.categories)
            .finish()
    }
}

use PluralCategory::{Few, Many, One, Other, Two, Zero};

/// CLDR `ja ko zh …` (and root): only `other`.
fn other(_: &PluralOperands) -> PluralCategory {
    Other
}

/// CLDR `ast de en et fi fy gl ia … nl sc sv sw ur yi`: `one: i = 1 and v = 0`.
fn one_if_integer_one(o: &PluralOperands) -> PluralCategory {
    if o.i == 1 && o.v == 0 { One } else { Other }
}

/// CLDR `af an asa az … nb … no … tr …`: `one: n = 1`.
fn one_if_one(o: &PluralOperands) -> PluralCategory {
    if o.is(1) { One } else { Other }
}

/// CLDR `da`: `one: n = 1 or t != 0 and i = 0,1`.
fn danish(o: &PluralOperands) -> PluralCategory {
    if o.is(1) || (o.t != 0 && o.i <= 1) {
        One
    } else {
        Other
    }
}

/// CLDR `es`: `one: n = 1`; `many: e = 0 and i != 0 and i % 1000000 = 0 and v = 0 or e != 0..5`.
fn spanish(o: &PluralOperands) -> PluralCategory {
    if o.is(1) {
        One
    } else if o.is_whole_millions() {
        Many
    } else {
        Other
    }
}

/// CLDR `ca it pt_PT vec`: `one: i = 1 and v = 0`; `many` as in [`spanish`].
fn catalan_italian(o: &PluralOperands) -> PluralCategory {
    if o.i == 1 && o.v == 0 {
        One
    } else if o.is_whole_millions() {
        Many
    } else {
        Other
    }
}

/// CLDR `fr`: `one: i = 0,1`; `many` as in [`spanish`].
fn french(o: &PluralOperands) -> PluralCategory {
    if o.i <= 1 {
        One
    } else if o.is_whole_millions() {
        Many
    } else {
        Other
    }
}

/// CLDR `pt`: `one: i = 0..1`; `many` as in [`spanish`].
fn portuguese(o: &PluralOperands) -> PluralCategory {
    french(o)
}

/// CLDR `ru uk`: `one: v = 0 and i % 10 = 1 and i % 100 != 11`;
/// `few: v = 0 and i % 10 = 2..4 and i % 100 != 12..14`;
/// `many: v = 0 and i % 10 = 0 or v = 0 and i % 10 = 5..9 or v = 0 and i % 100 = 11..14`.
fn russian(o: &PluralOperands) -> PluralCategory {
    if o.v != 0 {
        return Other;
    }
    let (i10, i100) = (o.i % 10, o.i % 100);
    if i10 == 1 && i100 != 11 {
        One
    } else if (2..=4).contains(&i10) && !(12..=14).contains(&i100) {
        Few
    } else {
        // Every other integer: i % 10 = 0 or 5..9, or i % 100 = 11..14.
        Many
    }
}

/// CLDR `pl`: `one: i = 1 and v = 0`; `few: v = 0 and i % 10 = 2..4 and i % 100 != 12..14`;
/// `many: v = 0 and i != 1 and i % 10 = 0..1 or v = 0 and i % 10 = 5..9 or v = 0 and
/// i % 100 = 12..14`.
fn polish(o: &PluralOperands) -> PluralCategory {
    if o.v != 0 {
        return Other;
    }
    let (i10, i100) = (o.i % 10, o.i % 100);
    if o.i == 1 {
        One
    } else if (2..=4).contains(&i10) && !(12..=14).contains(&i100) {
        Few
    } else {
        Many
    }
}

/// CLDR `cs sk`: `one: i = 1 and v = 0`; `few: i = 2..4 and v = 0`; `many: v != 0`.
fn czech(o: &PluralOperands) -> PluralCategory {
    match (o.i, o.v) {
        (_, 1..) => Many,
        (1, 0) => One,
        (2..=4, 0) => Few,
        _ => Other,
    }
}

/// CLDR `ar ars`: `zero: n = 0`; `one: n = 1`; `two: n = 2`; `few: n % 100 = 3..10`;
/// `many: n % 100 = 11..99`.
fn arabic(o: &PluralOperands) -> PluralCategory {
    if !o.is_integer() {
        return Other;
    }
    match (o.i, o.i % 100) {
        (0, _) => Zero,
        (1, _) => One,
        (2, _) => Two,
        (_, 3..=10) => Few,
        (_, 11..=99) => Many,
        _ => Other,
    }
}

/// CLDR `he` (since CLDR 42): `one: i = 1 and v = 0 or i = 0 and v != 0`;
/// `two: i = 2 and v = 0`.
fn hebrew(o: &PluralOperands) -> PluralCategory {
    match (o.i, o.v) {
        (1, 0) | (0, 1..) => One,
        (2, 0) => Two,
        _ => Other,
    }
}

/// CLDR `am as bn doi fa gu hi kn pcm zu`: `one: i = 0 or n = 1`.
fn hindi(o: &PluralOperands) -> PluralCategory {
    if o.i == 0 || o.is(1) { One } else { Other }
}
