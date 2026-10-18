//! Asset identity: object UUIDs, links between objects, and links to source files.
//!
//! These are the only types tools need to recognize to understand any content (see
//! `docs/design/0003-asset-pipeline.md`): every object has a [`Uuid`], objects refer to each other
//! through [`Link`]s and to binary source data through [`SourceFile`]s. Their reflected names are
//! pinned, so tools find them in any type's description by name.

use alloc::string::String;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;
use core::str::FromStr;

use marlin_reflect::Reflect;

/// A 128-bit object identifier (RFC 9562), written as `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
/// Tools create version 7 (time-ordered) UUIDs, which keep new objects' ids roughly sorted.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Reflect)]
#[reflect(name = "marlin_asset::Uuid")]
pub struct Uuid(pub u128);

impl Uuid {
    pub const NIL: Self = Self(0);

    pub const fn from_u128(value: u128) -> Self {
        Self(value)
    }

    pub const fn as_u128(self) -> u128 {
        self.0
    }

    pub const fn is_nil(self) -> bool {
        self.0 == 0
    }

    /// The RFC 9562 version field (7 for tool-created ids).
    pub const fn version(self) -> u8 {
        ((self.0 >> 76) & 0xf) as u8
    }

    /// A version 7 UUID from a Unix time in milliseconds and 74 random bits (the low 74 bits of
    /// `random`; the rest is ignored).
    pub const fn v7(unix_millis: u64, random: u128) -> Self {
        let rand_a = (random >> 62) & 0xfff;
        let rand_b = random & ((1 << 62) - 1);
        Self(
            ((unix_millis as u128 & 0xffff_ffff_ffff) << 80)
                | (0x7 << 76)
                | (rand_a << 64)
                | (0b10 << 62)
                | rand_b,
        )
    }

    /// A 64-bit hash for compact runtime tables (cook unit ids, cooked object tables).
    pub const fn hash64(self) -> u64 {
        // Fold the halves, then mix (splitmix64 finalizer) so time-ordered ids spread well.
        let mut x = (self.0 >> 64) as u64 ^ self.0 as u64;
        x ^= x >> 30;
        x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x ^= x >> 27;
        x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = self.0;
        write!(
            f,
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            (v >> 96) as u32,
            (v >> 80) as u16,
            (v >> 64) as u16,
            (v >> 48) as u16,
            v & 0xffff_ffff_ffff
        )
    }
}

impl fmt::Debug for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Uuid({self})")
    }
}

/// A string that is not a hyphenated UUID.
#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_error::Error)]
#[error("invalid UUID: expected xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx")]
pub struct ParseUuidError;

impl FromStr for Uuid {
    type Err = ParseUuidError;

    /// Accepts the hyphenated form only, either case.
    fn from_str(s: &str) -> Result<Self, ParseUuidError> {
        let bytes = s.as_bytes();
        if bytes.len() != 36 {
            return Err(ParseUuidError);
        }
        let mut value = 0u128;
        for (i, &b) in bytes.iter().enumerate() {
            if matches!(i, 8 | 13 | 18 | 23) {
                if b != b'-' {
                    return Err(ParseUuidError);
                }
                continue;
            }
            let digit = (b as char).to_digit(16).ok_or(ParseUuidError)?;
            value = (value << 4) | digit as u128;
        }
        Ok(Self(value))
    }
}

/// A reference from one asset object to another, by the target's [`Uuid`].
///
/// `T` is the target's type, checked by tools when content is loaded. Source text also records
/// the target's path and name for readability; only the UUID is authoritative, so links survive
/// moves and renames.
#[derive(Reflect)]
#[reflect(name = "marlin_asset::Link")]
pub struct Link<T> {
    pub target: Uuid,
    #[reflect(skip)]
    _target: PhantomData<fn() -> T>,
}

impl<T> Link<T> {
    pub const NONE: Self = Self::new(Uuid::NIL);

    pub const fn new(target: Uuid) -> Self {
        Self {
            target,
            _target: PhantomData,
        }
    }

    /// Whether the link points nowhere (the nil UUID).
    pub const fn is_none(&self) -> bool {
        self.target.is_nil()
    }
}

// Manual impls: derives would require `T` to implement them, but a link is just its UUID.
impl<T> Clone for Link<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Link<T> {}

impl<T> PartialEq for Link<T> {
    fn eq(&self, other: &Self) -> bool {
        self.target == other.target
    }
}

impl<T> Eq for Link<T> {}

impl<T> Hash for Link<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.target.hash(state);
    }
}

impl<T> Default for Link<T> {
    fn default() -> Self {
        Self::NONE
    }
}

impl<T> fmt::Debug for Link<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Link({})", self.target)
    }
}

/// A reference to a binary source file (an image, a glTF, a sound), as a path relative to the
/// asset root with `/` separators. Written `` `source:path` `` in source text. Cookers read the
/// file; it never reaches the runtime.
#[derive(Clone, PartialEq, Eq, Hash, Default, Debug, Reflect)]
#[reflect(name = "marlin_asset::SourceFile")]
pub struct SourceFile {
    pub path: String,
}

impl SourceFile {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

/// The key of a named object in name tables (0003 §4.4): xxh3-64 of its `!Name`. Only types
/// whose cooker registers them as named (shader packages) are in the tables, and a build whose
/// names collide does not cook, so a hit is the object.
pub fn name_hash(name: &str) -> u64 {
    marlin_hash::xxh3_64(name.as_bytes())
}
