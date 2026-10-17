//! Custom serializers for opaque types.
//!
//! The reflection format writes every kind but [`Opaque`](marlin_reflect::TypeKind::Opaque) on
//! its own. An opaque type is written:
//!
//! 1. with its [`SerialFns`], if it has them: static type data in its `TypeInfo`, or added to the
//!    global registry with [`register_custom`]. The value is opaque bytes, prefixed by their
//!    length and tagged with the type's id, so only the same type reads it back.
//! 2. else as its [`ReflectProxy`] value (a `Name` as a `String`, a `Handle` as a `u64`), exactly
//!    like a value of the proxy type, so a field can change between the type and its proxy.
//! 3. else not at all: [`Error::Unsupported`].
//!
//! Custom serializers only apply to opaque types; other kinds always use the format's own
//! encoding, which is what makes schema evolution work.

use alloc::vec::Vec;

use marlin_reflect::{
    IdCollision, Mut, Ref, Reflect, ReflectProxy, TypeInfo, find_data, type_info,
};

use crate::Error;

/// Writes an opaque value's bytes (appending to `out`).
pub type WriteFn = fn(value: Ref<'_>, out: &mut Vec<u8>) -> Result<(), Error>;
/// Reads an opaque value from exactly the bytes its `WriteFn` wrote.
pub type ReadFn = fn(value: Mut<'_>, bytes: &[u8]) -> Result<(), Error>;

/// Type data: the custom serializer of an opaque type.
#[derive(Clone, Copy)]
pub struct SerialFns {
    pub write: WriteFn,
    pub read: ReadFn,
}

impl SerialFns {
    pub const fn new(write: WriteFn, read: ReadFn) -> Self {
        Self { write, read }
    }

    /// The serializer of a [`CustomSerial`] type.
    pub const fn of<T: CustomSerial>() -> Self {
        Self {
            write: write_shim::<T>,
            read: read_shim::<T>,
        }
    }
}

/// Typed custom serialization; build its [`SerialFns`] with [`SerialFns::of`].
pub trait CustomSerial: Reflect {
    fn write_bytes(&self, out: &mut Vec<u8>) -> Result<(), Error>;
    /// Reads `bytes`, exactly what `write_bytes` wrote. Return [`Error::Incompatible`] for data
    /// that does not fit (the field then keeps its default), other errors for corrupt data.
    fn read_bytes(&mut self, bytes: &[u8]) -> Result<(), Error>;
}

fn write_shim<T: CustomSerial>(value: Ref<'_>, out: &mut Vec<u8>) -> Result<(), Error> {
    let info = value.info();
    value
        .downcast::<T>()
        .ok_or(Error::Unsupported(info.id))?
        .write_bytes(out)
}

fn read_shim<T: CustomSerial>(value: Mut<'_>, bytes: &[u8]) -> Result<(), Error> {
    value
        .downcast::<T>()
        .ok_or(Error::Incompatible)?
        .read_bytes(bytes)
}

/// Registers `T`'s [`CustomSerial`] impl in the global reflection registry.
pub fn register_custom<T: CustomSerial>() -> Result<(), IdCollision> {
    marlin_reflect::add_data(type_info::<T>(), SerialFns::of::<T>())
}

pub(crate) fn serial_fns(info: &'static TypeInfo) -> Option<&'static SerialFns> {
    find_data::<SerialFns>(info)
}

pub(crate) fn proxy(info: &'static TypeInfo) -> Option<&'static ReflectProxy> {
    find_data::<ReflectProxy>(info)
}
