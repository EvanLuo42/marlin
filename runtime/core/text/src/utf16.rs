//! UTF-16: what Win32, some console SDKs and many file formats speak.
//!
//! Encoding a `str` cannot fail, except for running out of room. Decoding can: UTF-16 from
//! outside may hold unpaired surrogates. The strict functions report the first one
//! ([`Utf16Error`]); the `_lossy` ones replace each with U+FFFD, as `String::from_utf16_lossy`
//! does.
//!
//! ```
//! use marlin_text::utf16;
//!
//! let mut buf = [0u16; 8];
//! let units = utf16::encode_to("a😀", &mut buf).unwrap();
//! assert_eq!(units, [0x61, 0xd83d, 0xde00]);
//! assert_eq!(utf16::decode_to_string(units).unwrap(), "a😀");
//! assert_eq!(utf16::decode_lossy(&[0x61, 0xd800]), "a\u{fffd}");
//! ```

use alloc::string::String;
use core::fmt;
use core::str::EncodeUtf16;

use marlin_containers::CapacityError;

/// An unpaired surrogate at unit `index`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_std::Error)]
#[error("unpaired surrogate {unit:#06x} at UTF-16 index {index}")]
pub struct Utf16Error {
    pub index: usize,
    pub unit: u16,
}

/// Why [`decode_to`] failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_std::Error)]
pub enum DecodeError {
    #[error(transparent)]
    Invalid(#[from] Utf16Error),
    /// The decoded text does not fit the buffer.
    #[error("the buffer is too small")]
    Capacity,
}

/// The number of UTF-16 units `s` encodes to.
pub fn len(s: &str) -> usize {
    // Every char takes one unit, four-byte (supplementary) ones two: count the lead bytes.
    s.chars().count() + s.bytes().filter(|&b| b >= 0xf0).count()
}

/// `s` as UTF-16 units: `str::encode_utf16`, here for discoverability.
pub fn encode(s: &str) -> EncodeUtf16<'_> {
    s.encode_utf16()
}

/// Encodes `s` into `out`; returns the units written. Fails, writing nothing, if `out` is
/// shorter than [`len(s)`](len).
pub fn encode_to<'a>(s: &str, out: &'a mut [u16]) -> Result<&'a mut [u16], CapacityError> {
    let n = len(s);
    if n > out.len() {
        return Err(CapacityError(()));
    }
    for (dst, unit) in out.iter_mut().zip(s.encode_utf16()) {
        *dst = unit;
    }
    Ok(&mut out[..n])
}

/// Like [`encode_to`], followed by a NUL unit, as Win32 wide-string parameters want. The
/// returned slice includes the NUL. `s` itself may contain NULs; the callee sees the first.
pub fn encode_to_nul_terminated<'a>(
    s: &str,
    out: &'a mut [u16],
) -> Result<&'a mut [u16], CapacityError> {
    let n = len(s);
    if n >= out.len() {
        return Err(CapacityError(()));
    }
    encode_to(s, &mut out[..n])?;
    out[n] = 0;
    Ok(&mut out[..=n])
}

/// The characters of `units`, or the error at each unpaired surrogate.
pub fn decode(units: &[u16]) -> Decode<'_> {
    Decode { units, index: 0 }
}

/// The iterator of [`decode`].
#[derive(Clone, Debug)]
pub struct Decode<'a> {
    units: &'a [u16],
    index: usize,
}

impl Iterator for Decode<'_> {
    type Item = Result<char, Utf16Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.index;
        let unit = *self.units.get(index)?;
        self.index += 1;
        // Not a surrogate: a BMP scalar value.
        if let Some(c) = char::from_u32(u32::from(unit)) {
            return Some(Ok(c));
        }
        let low = self.units.get(self.index).copied();
        let pair = match low {
            Some(low) if unit < 0xdc00 && (0xdc00..0xe000).contains(&low) => char::from_u32(
                0x10000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00),
            ),
            _ => None,
        };
        match pair {
            Some(c) => {
                self.index += 1;
                Some(Ok(c))
            }
            None => Some(Err(Utf16Error { index, unit })),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.units.len() - self.index;
        (left.div_ceil(2), Some(left))
    }
}

/// Checks that `units` is well-formed UTF-16.
pub fn validate(units: &[u16]) -> Result<(), Utf16Error> {
    decode(units).try_for_each(|c| c.map(drop))
}

/// The UTF-8 length of `units` once decoded; the error if it has an unpaired surrogate.
pub fn utf8_len(units: &[u16]) -> Result<usize, Utf16Error> {
    decode(units).try_fold(0, |n, c| Ok(n + c?.len_utf8()))
}

/// The UTF-8 length of `units` decoded with [`decode_lossy`].
pub fn utf8_len_lossy(units: &[u16]) -> usize {
    lossy(units).map(char::len_utf8).sum()
}

fn lossy(units: &[u16]) -> impl Iterator<Item = char> + '_ {
    decode(units).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
}

/// Decodes `units`, failing at the first unpaired surrogate.
pub fn decode_to_string(units: &[u16]) -> Result<String, Utf16Error> {
    let mut out = String::with_capacity(units.len());
    for c in decode(units) {
        out.push(c?);
    }
    Ok(out)
}

/// Decodes `units`, replacing each unpaired surrogate with U+FFFD.
pub fn decode_lossy(units: &[u16]) -> String {
    let mut out = String::with_capacity(units.len());
    out.extend(lossy(units));
    out
}

/// Decodes `units` into `out`, replacing each unpaired surrogate with U+FFFD.
pub fn decode_lossy_to(units: &[u16], out: &mut impl fmt::Write) -> fmt::Result {
    lossy(units).try_for_each(|c| out.write_char(c))
}

/// Decodes `units` into `buf` without allocating. Fails at the first unpaired surrogate, or
/// with [`DecodeError::Capacity`] (having written nothing useful) if the text does not fit.
pub fn decode_to<'a>(units: &[u16], buf: &'a mut [u8]) -> Result<&'a str, DecodeError> {
    let mut len = 0;
    for c in decode(units) {
        let c = c?;
        let end = len + c.len_utf8();
        if end > buf.len() {
            return Err(DecodeError::Capacity);
        }
        c.encode_utf8(&mut buf[len..end]);
        len = end;
    }
    // SAFETY: only whole encoded chars were written to `buf[..len]`.
    Ok(unsafe { core::str::from_utf8_unchecked(&buf[..len]) })
}
