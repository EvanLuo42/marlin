//! Locale-aware number formatting: decimal and grouping separators, grouping sizes, digits.

use core::fmt::{self, Write as _};

use super::Locale;
use crate::fmt::SliceWriter;

/// How a locale writes decimal numbers: CLDR's `decimalFormats` standard pattern (`#,##0.###`,
/// or `#,##,##0.###` for Indian grouping) and number symbols for the default numbering system.
///
/// The data is hand-encoded after CLDR (`common/main/<locale>.xml`) for the languages
/// [`for_locale`](Self::for_locale) lists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NumberFormat {
    pub decimal: &'static str,
    pub group: &'static str,
    pub minus: &'static str,
    /// The digit zero; the others follow it (`'0'`, or `'٠'` for Arabic-Indic digits).
    pub zero: char,
    /// Digits in the group nearest the decimal separator.
    pub primary_group: u8,
    /// Digits in each further group: 3, or 2 for Indian grouping (`12,34,567`).
    pub secondary_group: u8,
    /// Integer digits needed before grouping starts, beyond the primary group: CLDR's
    /// `minimumGroupingDigits`. With 2 (es, pl, pt-PT) `1000` stays ungrouped but `10 000` is
    /// grouped.
    pub min_grouping: u8,
}

const NBSP: &str = "\u{a0}";
/// CLDR uses U+202F NARROW NO-BREAK SPACE as the French group separator.
const NNBSP: &str = "\u{202f}";
/// U+2212 MINUS SIGN, the minus of sv, nb, fi.
const MINUS_SIGN: &str = "\u{2212}";

impl NumberFormat {
    /// `1,234,567.89`: CLDR root and English.
    pub const ROOT: Self = Self {
        decimal: ".",
        group: ",",
        minus: "-",
        zero: '0',
        primary_group: 3,
        secondary_group: 3,
        min_grouping: 1,
    };

    /// The format for `locale`. Covers en (en-IN with Indian grouping), de (de-CH, de-AT), nl,
    /// sv, da, nb/no, fi, es (es-MX, es-US, es-419), it, pt (pt-PT), fr, ru, uk, pl, cs,
    /// ar (Arabic-Indic digits), he, ja, ko, zh, tr and hi; other languages get [`ROOT`].
    ///
    /// [`ROOT`]: Self::ROOT
    pub fn for_locale(locale: &Locale) -> Self {
        let comma_dot = Self {
            decimal: ",",
            group: ".",
            ..Self::ROOT
        };
        let comma_space = Self {
            decimal: ",",
            group: NBSP,
            ..Self::ROOT
        };
        match (locale.language(), locale.region()) {
            ("en", Some("IN")) | ("hi", _) => Self {
                secondary_group: 2,
                ..Self::ROOT
            },
            ("de", Some("CH" | "LI")) => Self {
                group: "\u{2019}",
                ..Self::ROOT
            },
            ("de", Some("AT")) => comma_space,
            ("es", Some("MX" | "US" | "419")) => Self::ROOT,
            ("pt", Some("PT")) => Self {
                min_grouping: 2,
                ..comma_space
            },
            ("de" | "nl" | "da" | "it" | "pt" | "tr", _) => comma_dot,
            ("es", _) => Self {
                min_grouping: 2,
                ..comma_dot
            },
            ("sv" | "nb" | "no" | "nn" | "fi", _) => Self {
                minus: MINUS_SIGN,
                ..comma_space
            },
            ("fr", _) => Self {
                decimal: ",",
                group: NNBSP,
                ..Self::ROOT
            },
            ("ru" | "uk" | "cs", _) => comma_space,
            ("pl", _) => Self {
                min_grouping: 2,
                ..comma_space
            },
            // CLDR's default numbering system for Arabic is `arab`: ٠١٢٣٤٥٦٧٨٩, the Arabic
            // decimal and thousands separators, and a minus led by U+061C ARABIC LETTER MARK.
            ("ar", _) => Self {
                decimal: "\u{66b}",
                group: "\u{66c}",
                minus: "\u{61c}-",
                zero: '\u{660}',
                ..Self::ROOT
            },
            // Hebrew's minus carries U+200E LEFT-TO-RIGHT MARK.
            ("he", _) => Self {
                minus: "\u{200e}-",
                ..Self::ROOT
            },
            _ => Self::ROOT,
        }
    }

    /// Writes `n`, grouped.
    pub fn format_int(&self, n: i64, out: &mut (impl fmt::Write + ?Sized)) -> fmt::Result {
        let mut buf = [0u8; 20];
        let digits = ascii_digits(n.unsigned_abs(), &mut buf);
        self.write_parts(n < 0, digits, "", out)
    }

    /// Writes `x` rounded to at most `max_fraction` fraction digits (half to even on the exact
    /// binary value, as `core::fmt` does), with trailing zeros dropped down to `min_fraction`.
    /// At most 20 fraction digits. A value that rounds to zero has no minus sign. Infinity is
    /// `∞`, not-a-number `NaN`.
    pub fn format_float(
        &self,
        x: f64,
        min_fraction: u8,
        max_fraction: u8,
        out: &mut (impl fmt::Write + ?Sized),
    ) -> fmt::Result {
        if x.is_nan() {
            return out.write_str("NaN");
        }
        if x.is_infinite() {
            if x < 0.0 {
                out.write_str(self.minus)?;
            }
            return out.write_char('∞');
        }
        let mut buf = [0u8; 340];
        let text = decimal_digits(x, min_fraction, max_fraction, &mut buf);
        let (int, frac) = text.split_once('.').unwrap_or((text, ""));
        let zero = int.bytes().chain(frac.bytes()).all(|b| b == b'0');
        self.write_parts(x < 0.0 && !zero, int, frac, out)
    }

    /// Writes a number given as ASCII digits: the integer part (no leading zeros except a lone
    /// `0`) and the fraction digits, which may be empty.
    pub fn write_parts(
        &self,
        negative: bool,
        int: &str,
        frac: &str,
        out: &mut (impl fmt::Write + ?Sized),
    ) -> fmt::Result {
        if negative {
            out.write_str(self.minus)?;
        }
        let len = int.len();
        let (primary, secondary) = (self.primary_group as usize, self.secondary_group as usize);
        let grouped = primary > 0 && len >= primary + self.min_grouping as usize;
        let mut start = 0;
        for i in 1..len {
            let left = len - i;
            if grouped && left >= primary && (left - primary).is_multiple_of(secondary.max(1)) {
                write_digits(self.zero, &int[start..i], out)?;
                out.write_str(self.group)?;
                start = i;
            }
        }
        write_digits(self.zero, &int[start..], out)?;
        if !frac.is_empty() {
            out.write_str(self.decimal)?;
            write_digits(self.zero, frac, out)?;
        }
        Ok(())
    }
}

/// `n` in ASCII decimal, in `buf`.
pub(crate) fn ascii_digits(mut n: u64, buf: &mut [u8; 20]) -> &str {
    let mut start = buf.len();
    loop {
        start -= 1;
        buf[start] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    // Only ASCII digits were written.
    core::str::from_utf8(&buf[start..]).unwrap_or("0")
}

/// Writes the ASCII `digits` in the digit set starting at `zero`.
pub(crate) fn write_digits(
    zero: char,
    digits: &str,
    out: &mut (impl fmt::Write + ?Sized),
) -> fmt::Result {
    if zero == '0' {
        return out.write_str(digits);
    }
    for b in digits.bytes() {
        let value = u32::from(b.wrapping_sub(b'0')).min(9);
        out.write_char(char::from_u32(zero as u32 + value).unwrap_or('?'))?;
    }
    Ok(())
}

/// `|x|` (finite) in ASCII, `int.frac`, rounded to `max_fraction` digits and trimmed of
/// trailing zeros down to `min_fraction`.
pub(crate) fn decimal_digits(x: f64, min_fraction: u8, max_fraction: u8, buf: &mut [u8]) -> &str {
    let max = usize::from(max_fraction.min(20));
    let min = usize::from(min_fraction).min(max);
    let mut w = SliceWriter::new(buf);
    // 309 integer digits at most, a point and 20 digits: fits in 340 bytes.
    let _ = write!(w, "{:.*}", max, x.abs());
    let text = w.into_str();
    let Some(point) = text.find('.') else {
        return text;
    };
    let frac = &text[point + 1..];
    let keep = frac.trim_end_matches('0').len().max(min);
    if keep == 0 {
        &text[..point]
    } else {
        &text[..point + 1 + keep]
    }
}
