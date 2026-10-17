//! `Guid`: a 128-bit identifier in the RFC 9562 (UUID) layout.

use core::{fmt, str};

use crate::Xxh3;

/// A 128-bit globally unique id, stored as a `u128` whose big-endian bytes are the UUID's bytes in
/// their canonical (string) order. So `Ord` sorts like the strings do, and `to_bytes` /
/// `from_bytes` match every other UUID library.
///
/// Text form: the canonical `8-4-4-4-12` hex groups, `67e55044-10b1-426f-9247-bb680e5fe0c8`.
/// [`Display`](fmt::Display) writes lowercase; [`parse`](Self::parse) accepts either case.
///
/// Random GUIDs come from the tools (`masset` issues UUIDv7s); there is no generator here, since
/// L2 has no RNG. [`from_name`](Self::from_name) derives one deterministically.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct Guid(u128);

/// [`Guid::parse`] got something other than `8-4-4-4-12` hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseGuidError;

impl fmt::Display for ParseGuidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid GUID: expected 8-4-4-4-12 hex digits")
    }
}

impl core::error::Error for ParseGuidError {}

/// Where the hyphens go in the 36-character form.
const HYPHENS: [usize; 4] = [8, 13, 18, 23];

impl Guid {
    /// All zeros: "no id".
    pub const NIL: Self = Self(0);

    #[inline]
    pub const fn from_u128(value: u128) -> Self {
        Self(value)
    }

    #[inline]
    pub const fn as_u128(self) -> u128 {
        self.0
    }

    /// From the 16 bytes in canonical order (the order of the hex digits).
    #[inline]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(u128::from_be_bytes(bytes))
    }

    /// The 16 bytes in canonical order.
    #[inline]
    pub const fn to_bytes(self) -> [u8; 16] {
        self.0.to_be_bytes()
    }

    #[inline]
    pub const fn is_nil(self) -> bool {
        self.0 == 0
    }

    /// The RFC 9562 version nibble: 7 for `masset`'s time-ordered ids, 8 for
    /// [`from_name`](Self::from_name). Meaningless for ids that are not UUIDs.
    #[inline]
    pub const fn version(self) -> u8 {
        ((self.0 >> 76) & 0xf) as u8
    }

    /// A GUID derived from a namespace GUID and a name: the same inputs always give the same id,
    /// on every platform and in every version.
    ///
    /// This is NOT an RFC 4122 / 9562 version-5 (SHA-1) UUID and will not match one. It is
    /// `xxh3_128(namespace.to_bytes() ++ name)`, viewed big-endian, with the version nibble set
    /// to 8 and the variant bits to `0b10`: a valid RFC 9562 version-8 ("custom") UUID, so it can
    /// never equal a v7 id. Not cryptographic: anyone can construct names that collide on
    /// purpose, so derive ids only from names the project controls.
    pub fn from_name(namespace: Guid, name: &[u8]) -> Self {
        let mut h = Xxh3::new();
        h.update(&namespace.to_bytes());
        h.update(name);
        const VERSION: u128 = 0xf << 76;
        const VARIANT: u128 = 0b11 << 62;
        Self((h.digest128() & !VERSION & !VARIANT) | (0x8 << 76) | (0b10 << 62))
    }

    /// Parses the canonical `8-4-4-4-12` form, hex digits in either case.
    pub const fn parse(s: &str) -> Result<Self, ParseGuidError> {
        let b = s.as_bytes();
        if b.len() != 36 {
            return Err(ParseGuidError);
        }
        let mut value = 0u128;
        let mut i = 0;
        while i < 36 {
            if i == HYPHENS[0] || i == HYPHENS[1] || i == HYPHENS[2] || i == HYPHENS[3] {
                if b[i] != b'-' {
                    return Err(ParseGuidError);
                }
            } else {
                let digit = match b[i] {
                    c @ b'0'..=b'9' => c - b'0',
                    c @ b'a'..=b'f' => c - b'a' + 10,
                    c @ b'A'..=b'F' => c - b'A' + 10,
                    _ => return Err(ParseGuidError),
                };
                value = (value << 4) | digit as u128;
            }
            i += 1;
        }
        Ok(Self(value))
    }

    /// The canonical lowercase text form as ASCII bytes, without allocating.
    pub const fn to_hyphenated(self) -> [u8; 36] {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = [b'-'; 36];
        let mut i = 0;
        let mut shift = 128;
        while i < 36 {
            if !(i == HYPHENS[0] || i == HYPHENS[1] || i == HYPHENS[2] || i == HYPHENS[3]) {
                shift -= 4;
                out[i] = HEX[((self.0 >> shift) & 0xf) as usize];
            }
            i += 1;
        }
        out
    }
}

impl From<u128> for Guid {
    #[inline]
    fn from(value: u128) -> Self {
        Self(value)
    }
}

impl From<Guid> for u128 {
    #[inline]
    fn from(guid: Guid) -> Self {
        guid.0
    }
}

impl str::FromStr for Guid {
    type Err = ParseGuidError;

    #[inline]
    fn from_str(s: &str) -> Result<Self, ParseGuidError> {
        Self::parse(s)
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = self.to_hyphenated();
        f.pad(str::from_utf8(&text).map_err(|_| fmt::Error)?)
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Guid({self})")
    }
}
