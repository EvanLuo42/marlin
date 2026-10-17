//! Binary serialization and zero-copy cooked data layouts.
//!
//! Two independent tools:
//!
//! - **Reflection-driven serialization** ([`to_bytes`], [`from_bytes`], [`read_into`],
//!   [`read_value`], and [`stream`] for reading from a file incrementally) for any [`Reflect`]
//!   type: save games, editor assets, anything whose types evolve between writing and reading.
//! - **Cooked layouts** ([`cooked`]): `#[repr(C)]` [`Pod`](cooked::Pod) data with relative
//!   pointers, written once by a cooker and read in place from a loaded or mmapped buffer.
//!
//! # Format (version 2)
//!
//! Little-endian, fixed-width integers; writing the same value always gives the same bytes (hash
//! maps and sets are written sorted by their encoded entries).
//!
//! ```text
//! file    = "MRLS" version:u16 flags:u16(=0) root_type_id:u64 sig value
//! sig     = kind:u8                         scalar, string, struct, tuple or enum
//!         | LIST sig | OPTION sig | SET sig | ARRAY len:u32 sig | MAP sig sig
//!         | CUSTOM type_id:u64              opaque type with a custom serializer
//!         | VERSIONED version:u32 kind:u8   struct or enum with a data version
//! value   = scalar                          bool as 0/1, char as u32, usize/isize as 64-bit
//!         | len:u32 utf8                    string
//!         | fields                          struct, tuple (fields "0", "1", ...)
//!         | variant_hash:u32 fields         enum (variant by name hash)
//!         | count:u32 value*                list, set
//!         | value*                          array (length from the signature)
//!         | count:u32 (value value)*        map (key, value)
//!         | 0 | 1 value                     option
//!         | len:u32 bytes                   custom
//! fields  = count:u32 (name_hash:u32 len:u32 sig value)*
//! ```
//!
//! Pointers (`Box`, `Rc`, `Arc`) are written as their pointee, and opaque types with a
//! [`ReflectProxy`](marlin_reflect::ReflectProxy) as their proxy, so neither appears in the
//! format. Opaque types are written by their custom serializer if they have one; see [`custom`].
//! Version 1 data (the same without tuples, maps, sets, custom values and versions) is still
//! read.
//!
//! Fields carry their name hash, byte length and signature, which is what makes evolution work:
//! when reading, fields are matched by name hash, so fields can be added, removed and reordered;
//! unknown fields are skipped and missing ones keep their default. Fields whose type changed are
//! converted when possible (between any numeric types, `T` <-> `Option<T>`, `T` <-> `Box<T>`,
//! arrays, lists and sets, structs and tuples). A field whose type changed incompatibly, a number
//! that no longer fits, a `None` read into a non-optional field and a removed enum variant all
//! leave the value as it was: when loading with [`from_bytes`], that is the default. A field that
//! fails for lack of a default (a list of items without one, say) is reset to its own type's
//! default. Changes beyond that are handled by explicit [`migrate`]ions between data versions.
//!
//! Corrupt or truncated input returns an [`Error`]; it never panics or reads out of bounds. An
//! element count is checked against the bytes left before anything is allocated for it, so a
//! list never grows beyond the number of elements the input can actually hold. Nesting is limited
//! by [`ReadOptions::max_depth`].
//!
//! Lengths and counts are 32-bit, so a string, list or field holds at most 4 GiB; cooked
//! relative offsets are 32-bit signed (2 GiB). All supported targets are little-endian, which
//! the crate checks at compile time; cooked data relies on it.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod cooked;
pub mod custom;
pub mod migrate;
mod read;
pub mod stream;
mod wire;
mod write;

use alloc::vec::Vec;
use core::fmt;

use marlin_reflect::{Mut, Ref, Reflect, Registry, TypeId, Value};

use crate::read::Reader;
use crate::wire::Input;

pub use custom::{CustomSerial, SerialFns, register_custom};
pub use migrate::{Migrate, MigrateFn, OldFields, register_migration};

const _: () = assert!(
    cfg!(target_endian = "little"),
    "marlin-serial assumes a little-endian target: cooked data is used in place without byte \
     swapping"
);

/// First bytes of every serialized value.
pub const MAGIC: [u8; 4] = *b"MRLS";
/// Format version written by this crate; readers accept versions up to it.
pub const VERSION: u16 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    // Malformed input.
    /// Not serialized data.
    BadMagic,
    /// Written by a newer format version, or with flags this version does not know.
    UnsupportedVersion(u16),
    /// The input ends early, or a length or count exceeds the input.
    Truncated,
    InvalidSignature,
    InvalidBool,
    InvalidChar,
    InvalidUtf8,
    /// An option tag other than 0 or 1.
    InvalidTag,
    /// A field's length does not match its contents.
    InvalidLength,
    /// Bytes after the root value.
    TrailingBytes,
    /// Nesting deeper than [`ReadOptions::max_depth`].
    TooDeep,

    // Data that is well-formed but does not fit the reading type.
    /// The root value (or a field, internally) cannot be read as the target type.
    Incompatible,
    /// A value had to be built but its type has no default (see `Reflect::reflect_default`).
    NoDefault,
    /// [`read_value`] found a root type the registry does not know.
    UnknownType(TypeId),

    // Writing.
    /// The type (or a field's type) cannot be serialized, such as an opaque type without a
    /// custom serializer or proxy.
    Unsupported(TypeId),
    /// A string, list or field longer than `u32::MAX` bytes or elements.
    TooLarge,

    /// Reading a [`stream::Source`] failed.
    Io(marlin_std::io::Error),
}

impl Error {
    /// Mismatches between data and type, which a field can recover from by using its default.
    fn is_schema(self) -> bool {
        matches!(self, Self::Incompatible | Self::NoDefault)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => f.write_str("not serialized data (bad magic)"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported format version {v}"),
            Self::Truncated => f.write_str("input is truncated"),
            Self::InvalidSignature => f.write_str("invalid type signature"),
            Self::InvalidBool => f.write_str("invalid bool"),
            Self::InvalidChar => f.write_str("invalid char"),
            Self::InvalidUtf8 => f.write_str("invalid UTF-8 in string"),
            Self::InvalidTag => f.write_str("invalid option tag"),
            Self::InvalidLength => f.write_str("field length does not match its contents"),
            Self::TrailingBytes => f.write_str("trailing bytes after the value"),
            Self::TooDeep => f.write_str("nesting too deep"),
            Self::Incompatible => f.write_str("data does not match the target type"),
            Self::NoDefault => f.write_str("a value without default construction was needed"),
            Self::UnknownType(id) => write!(f, "unregistered type {id}"),
            Self::Unsupported(id) => write!(f, "type {id} cannot be serialized"),
            Self::TooLarge => f.write_str("value too large for the format"),
            Self::Io(e) => write!(f, "reading serialized data: {e}"),
        }
    }
}

impl core::error::Error for Error {}

#[derive(Clone, Copy, Debug)]
pub struct ReadOptions {
    /// Maximum nesting of values (and of type signatures). Bounds stack use on malicious input.
    pub max_depth: u32,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self { max_depth: 128 }
    }
}

/// Appends `value` to `out`. On error, `out` is left as it was.
pub fn write(value: Ref<'_>, out: &mut Vec<u8>) -> Result<(), Error> {
    let start = out.len();
    let result = write_unchecked(value, out);
    if result.is_err() {
        out.truncate(start);
    }
    result
}

fn write_unchecked(value: Ref<'_>, out: &mut Vec<u8>) -> Result<(), Error> {
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&value.info().id.0.to_le_bytes());
    write::write_sig(out, value.info())?;
    write::write_value(out, value)
}

pub fn to_bytes<T: Reflect>(value: &T) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    write(Ref::new(value), &mut out)?;
    Ok(out)
}

/// The fixed-size start of serialized data.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    pub version: u16,
    /// Stable id of the type that was written.
    pub root: TypeId,
}

/// Size of the fixed header: magic, version, flags and root type id.
pub(crate) const HEADER_LEN: usize = 16;

fn header(input: &mut Input<'_>) -> Result<Header, Error> {
    if input.array::<4>().map_err(|_| Error::BadMagic)? != MAGIC {
        return Err(Error::BadMagic);
    }
    let version = input.u16()?;
    let flags = input.u16()?;
    if version == 0 || version > VERSION || flags != 0 {
        return Err(Error::UnsupportedVersion(version));
    }
    let root = TypeId(input.u64()?);
    Ok(Header { version, root })
}

pub fn read_header(bytes: &[u8]) -> Result<Header, Error> {
    header(&mut Input::new(bytes))
}

/// Reads `bytes` into an existing value. Fields absent from the data keep their current value.
/// On error the target may be partly updated.
pub fn read_into(target: Mut<'_>, bytes: &[u8]) -> Result<(), Error> {
    read_into_with(target, bytes, &ReadOptions::default())
}

pub fn read_into_with(target: Mut<'_>, bytes: &[u8], options: &ReadOptions) -> Result<(), Error> {
    let mut input = Input::new(bytes);
    header(&mut input)?;
    let sig = input.sig(options.max_depth)?;
    if !wire::compatible(target.info(), sig) {
        return Err(Error::Incompatible);
    }
    let reader = Reader {
        max_depth: options.max_depth,
    };
    reader.decode(&mut input, target, sig, options.max_depth)?;
    if input.remaining() != 0 {
        return Err(Error::TrailingBytes);
    }
    Ok(())
}

/// Reads a `T`, starting from its default so missing fields get default values.
pub fn from_bytes<T: Reflect>(bytes: &[u8]) -> Result<T, Error> {
    let mut value = T::reflect_default().ok_or(Error::NoDefault)?;
    read_into(Mut::new(&mut value), bytes)?;
    Ok(value)
}

/// Reads a value of whatever type was written, looking it up in `registry` by its stable id.
pub fn read_value(bytes: &[u8], registry: &Registry) -> Result<Value, Error> {
    let root = read_header(bytes)?.root;
    let info = registry.get(root).ok_or(Error::UnknownType(root))?;
    let mut value = Value::default_of(info).ok_or(Error::NoDefault)?;
    read_into(value.as_mut(), bytes)?;
    Ok(value)
}
