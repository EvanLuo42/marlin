//! `Locale`: a BCP 47 language tag reduced to language, script and region.

use core::fmt;
use core::str;

/// A language with an optional script and region: `en`, `pt-BR`, `zh-Hans-CN`, `es-419`.
///
/// Parsed from BCP 47 tags ([`Locale::parse`]), case-insensitively and with `-` or `_` between
/// subtags, and stored canonically cased (`zh-Hant-TW`). Variants, extensions and private-use
/// subtags are checked for syntax and dropped: `de-DE-u-co-phonebk` is `de-DE`. `Copy`, 10 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Locale {
    /// 2 or 3 lowercase letters, zero-padded.
    language: [u8; 3],
    /// 4 letters, titlecase, or zeros.
    script: [u8; 4],
    /// 2 uppercase letters or 3 digits, zero-padded, or zeros.
    region: [u8; 3],
}

/// Why a tag is not a [`Locale`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_std::Error)]
pub enum LocaleError {
    #[error("empty language tag")]
    Empty,
    /// The language subtag is not 2 or 3 letters.
    #[error("bad language subtag")]
    Language,
    /// The subtag starting at this byte offset is malformed or out of place.
    #[error("bad subtag at byte {0}")]
    Subtag(usize),
}

impl Locale {
    /// The root locale, `und`: CLDR's defaults, and what every fallback chain ends in.
    pub const ROOT: Self = Self {
        language: *b"und",
        script: [0; 4],
        region: [0; 3],
    };

    /// Parses a BCP 47 language tag: `language[-script][-region][-variant…][-extension…]`.
    pub fn parse(tag: &str) -> Result<Self, LocaleError> {
        if tag.is_empty() {
            return Err(LocaleError::Empty);
        }
        let mut subtags = Subtags { tag, pos: 0 };
        let (_, language) = subtags.next().ok_or(LocaleError::Empty)?;
        if !(2..=3).contains(&language.len()) || !language.bytes().all(|b| b.is_ascii_alphabetic())
        {
            return Err(LocaleError::Language);
        }
        let mut locale = Self {
            language: [0; 3],
            script: [0; 4],
            region: [0; 3],
        };
        for (dst, b) in locale.language.iter_mut().zip(language.bytes()) {
            *dst = b.to_ascii_lowercase();
        }
        // 0: script next, 1: region next, 2: variants, 3: extensions.
        let mut stage = 0;
        for (at, subtag) in subtags {
            let b = subtag.as_bytes();
            let alpha = b.iter().all(u8::is_ascii_alphabetic);
            let digits = b.iter().all(u8::is_ascii_digit);
            let alnum = b.iter().all(u8::is_ascii_alphanumeric);
            match subtag.len() {
                _ if b.is_empty() || !alnum || b.len() > 8 => return Err(LocaleError::Subtag(at)),
                4 if stage == 0 && alpha => {
                    for (i, (dst, &c)) in locale.script.iter_mut().zip(b).enumerate() {
                        *dst = if i == 0 {
                            c.to_ascii_uppercase()
                        } else {
                            c.to_ascii_lowercase()
                        };
                    }
                    stage = 1;
                }
                2 if stage <= 1 && alpha => {
                    for (dst, &c) in locale.region.iter_mut().zip(b) {
                        *dst = c.to_ascii_uppercase();
                    }
                    stage = 2;
                }
                3 if stage <= 1 && digits => {
                    locale.region.copy_from_slice(b);
                    stage = 2;
                }
                // Variants: 5-8 characters, or a digit and 3 more.
                5..=8 if stage <= 2 => stage = 2,
                4 if stage <= 2 && b[0].is_ascii_digit() => stage = 2,
                // An extension or private-use singleton: everything after it is dropped.
                1 => stage = 3,
                _ if stage == 3 => {}
                _ => return Err(LocaleError::Subtag(at)),
            }
        }
        Ok(locale)
    }

    /// The language subtag (`"pt"`).
    pub fn language(&self) -> &str {
        ascii(&self.language)
    }

    /// The script subtag (`"Hans"`), if any.
    pub fn script(&self) -> Option<&str> {
        (self.script[0] != 0).then(|| ascii(&self.script))
    }

    /// The region subtag (`"BR"`, `"419"`), if any.
    pub fn region(&self) -> Option<&str> {
        (self.region[0] != 0).then(|| ascii(&self.region))
    }

    pub fn is_root(&self) -> bool {
        *self == Self::ROOT
    }

    /// This locale without its region.
    pub fn without_region(self) -> Self {
        Self {
            region: [0; 3],
            ..self
        }
    }

    /// The next locale to try when this one has no data, or `None` at the end of the chain.
    ///
    /// Truncation (`zh-Hans-CN` → `zh-Hans` → `zh`, `pt-BR` → `pt`), with two rules from CLDR's
    /// parent locales: Chinese in Taiwan, Hong Kong and Macao goes to `zh-Hant`, and `zh-Hant`
    /// has no parent: Traditional Chinese never falls back to (Simplified) `zh`.
    pub fn parent(&self) -> Option<Self> {
        let zh = self.language() == "zh";
        if self.region[0] != 0 {
            if zh && self.script[0] == 0 && matches!(self.region(), Some("TW" | "HK" | "MO")) {
                return Some(Self {
                    script: *b"Hant",
                    region: [0; 3],
                    ..*self
                });
            }
            return Some(self.without_region());
        }
        if self.script[0] != 0 {
            if zh && self.script() == Some("Hant") {
                return None;
            }
            return Some(Self {
                script: [0; 4],
                ..*self
            });
        }
        None
    }

    /// This locale, then each [`parent`](Self::parent): the order to look for strings in.
    pub fn fallback_chain(&self) -> FallbackChain {
        FallbackChain { next: Some(*self) }
    }
}

impl Default for Locale {
    fn default() -> Self {
        Self::ROOT
    }
}

impl str::FromStr for Locale {
    type Err = LocaleError;

    fn from_str(s: &str) -> Result<Self, LocaleError> {
        Self::parse(s)
    }
}

impl fmt::Display for Locale {
    /// The canonical tag: `zh-Hant-TW`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.language())?;
        for part in [self.script(), self.region()].into_iter().flatten() {
            f.write_str("-")?;
            f.write_str(part)?;
        }
        Ok(())
    }
}

impl fmt::Debug for Locale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Locale(\"{self}\")")
    }
}

/// The iterator of [`Locale::fallback_chain`].
#[derive(Clone, Debug)]
pub struct FallbackChain {
    next: Option<Locale>,
}

impl Iterator for FallbackChain {
    type Item = Locale;

    fn next(&mut self) -> Option<Locale> {
        let locale = self.next?;
        self.next = locale.parent();
        Some(locale)
    }
}

fn ascii(bytes: &[u8]) -> &str {
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    // Only ASCII letters and digits are ever stored.
    str::from_utf8(&bytes[..len]).unwrap_or("")
}

/// The subtags of a tag with their byte offsets; an empty subtag (`en--US`) is returned as
/// such so that it fails the syntax check.
struct Subtags<'a> {
    tag: &'a str,
    pos: usize,
}

impl<'a> Iterator for Subtags<'a> {
    type Item = (usize, &'a str);

    fn next(&mut self) -> Option<(usize, &'a str)> {
        if self.pos > self.tag.len() {
            return None;
        }
        let start = self.pos;
        let rest = &self.tag[start..];
        let len = rest.find(['-', '_']).unwrap_or(rest.len());
        self.pos = start + len + 1;
        Some((start, &rest[..len]))
    }
}
