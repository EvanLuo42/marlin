//! Wire-level pieces shared by the reader and writer: type signatures and the input cursor.
//!
//! A signature describes how a value is encoded, without names: a kind byte, followed for
//! containers by the element's signature (and an array's length). Structs and enums are
//! self-describing (their fields carry name hashes and lengths), so their signature is one byte
//! and does not recurse into fields. That keeps signatures short and finite for recursive types.

use marlin_reflect::{Primitive, TypeInfo, TypeKind};

use crate::Error;

pub(crate) const BOOL: u8 = 0x01;
pub(crate) const CHAR: u8 = 0x02;
pub(crate) const U8: u8 = 0x03;
pub(crate) const U16: u8 = 0x04;
pub(crate) const U32: u8 = 0x05;
pub(crate) const U64: u8 = 0x06;
pub(crate) const U128: u8 = 0x07;
pub(crate) const I8: u8 = 0x08;
pub(crate) const I16: u8 = 0x09;
pub(crate) const I32: u8 = 0x0a;
pub(crate) const I64: u8 = 0x0b;
pub(crate) const I128: u8 = 0x0c;
pub(crate) const F32: u8 = 0x0d;
pub(crate) const F64: u8 = 0x0e;
pub(crate) const STRING: u8 = 0x10;
pub(crate) const STRUCT: u8 = 0x11;
pub(crate) const ENUM: u8 = 0x12;
/// Followed by the element signature; the value starts with a `u32` count.
pub(crate) const LIST: u8 = 0x13;
/// Followed by a `u32` length and the element signature; the value is just the elements.
pub(crate) const ARRAY: u8 = 0x14;
/// Followed by the inner signature; the value is a `u8` tag (0 or 1) and, if 1, the inner value.
pub(crate) const OPTION: u8 = 0x15;

/// Wire kind of a primitive. `usize`/`isize` are always 64-bit on the wire.
pub(crate) fn primitive_kind(p: Primitive) -> u8 {
    match p {
        Primitive::Bool => BOOL,
        Primitive::Char => CHAR,
        Primitive::U8 => U8,
        Primitive::U16 => U16,
        Primitive::U32 => U32,
        Primitive::U64 | Primitive::Usize => U64,
        Primitive::U128 => U128,
        Primitive::I8 => I8,
        Primitive::I16 => I16,
        Primitive::I32 => I32,
        Primitive::I64 | Primitive::Isize => I64,
        Primitive::I128 => I128,
        Primitive::F32 => F32,
        Primitive::F64 => F64,
    }
}

/// Encoded size of a scalar kind, or `None` for other kinds.
pub(crate) fn scalar_size(kind: u8) -> Option<usize> {
    Some(match kind {
        BOOL | U8 | I8 => 1,
        U16 | I16 => 2,
        CHAR | U32 | I32 | F32 => 4,
        U64 | I64 | F64 => 8,
        U128 | I128 => 16,
        _ => return None,
    })
}

pub(crate) fn is_numeric(kind: u8) -> bool {
    matches!(kind, U8..=F64)
}

/// A validated signature: the bytes from its kind byte to its end.
#[derive(Clone, Copy)]
pub(crate) struct Sig<'a>(&'a [u8]);

impl<'a> Sig<'a> {
    pub(crate) fn kind(self) -> u8 {
        self.0[0]
    }

    /// Element signature of a list, array or option.
    pub(crate) fn element(self) -> Sig<'a> {
        match self.kind() {
            ARRAY => Sig(&self.0[5..]),
            _ => Sig(&self.0[1..]),
        }
    }

    pub(crate) fn array_len(self) -> u32 {
        u32::from_le_bytes([self.0[1], self.0[2], self.0[3], self.0[4]])
    }

    /// Smallest possible encoding of a value with this signature, used to reject element counts
    /// the remaining input cannot hold before allocating for them.
    pub(crate) fn min_size(self) -> usize {
        match self.kind() {
            STRING | STRUCT | LIST => 4,
            ENUM => 8,
            OPTION => 1,
            ARRAY => (self.array_len() as usize).saturating_mul(self.element().min_size()),
            kind => scalar_size(kind).unwrap_or(1),
        }
    }
}

/// Bounds-checked little-endian reader over untrusted bytes.
pub(crate) struct Input<'a> {
    bytes: &'a [u8],
}

impl<'a> Input<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len()
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if n > self.bytes.len() {
            return Err(Error::Truncated);
        }
        let (head, tail) = self.bytes.split_at(n);
        self.bytes = tail;
        Ok(head)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        Ok(self.take(N)?.try_into().unwrap_or([0; N]))
    }

    pub(crate) fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.array::<1>()?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, Error> {
        self.array().map(u16::from_le_bytes)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, Error> {
        self.array().map(u32::from_le_bytes)
    }

    pub(crate) fn u64(&mut self) -> Result<u64, Error> {
        self.array().map(u64::from_le_bytes)
    }

    /// Reads a signature, validating it completely.
    pub(crate) fn sig(&mut self, max_depth: u32) -> Result<Sig<'a>, Error> {
        let start = self.bytes;
        self.skip_sig(max_depth)?;
        let len = start.len() - self.bytes.len();
        Ok(Sig(&start[..len]))
    }

    fn skip_sig(&mut self, depth: u32) -> Result<(), Error> {
        if depth == 0 {
            return Err(Error::TooDeep);
        }
        match self.u8()? {
            LIST | OPTION => self.skip_sig(depth - 1),
            ARRAY => {
                self.u32()?;
                self.skip_sig(depth - 1)
            }
            STRING | STRUCT | ENUM => Ok(()),
            kind if scalar_size(kind).is_some() => Ok(()),
            _ => Err(Error::InvalidSignature),
        }
    }

    /// Reads a `u32` element count and checks the input can hold that many elements.
    pub(crate) fn count(&mut self, element: Sig<'_>) -> Result<usize, Error> {
        let count = self.u32()? as usize;
        self.check_count(count, element)?;
        Ok(count)
    }

    pub(crate) fn check_count(&self, count: usize, element: Sig<'_>) -> Result<(), Error> {
        if count.saturating_mul(element.min_size().max(1)) > self.remaining() {
            return Err(Error::Truncated);
        }
        Ok(())
    }

    /// Skips a value with signature `sig`.
    pub(crate) fn skip_value(&mut self, sig: Sig<'_>, depth: u32) -> Result<(), Error> {
        if depth == 0 {
            return Err(Error::TooDeep);
        }
        match sig.kind() {
            STRING => {
                let len = self.u32()? as usize;
                self.take(len)?;
            }
            STRUCT => self.skip_fields()?,
            ENUM => {
                self.u32()?;
                self.skip_fields()?;
            }
            LIST => {
                let element = sig.element();
                for _ in 0..self.count(element)? {
                    self.skip_value(element, depth - 1)?;
                }
            }
            ARRAY => {
                let element = sig.element();
                let len = sig.array_len() as usize;
                self.check_count(len, element)?;
                for _ in 0..len {
                    self.skip_value(element, depth - 1)?;
                }
            }
            OPTION => match self.u8()? {
                0 => {}
                1 => self.skip_value(sig.element(), depth - 1)?,
                _ => return Err(Error::InvalidTag),
            },
            kind => {
                // Validated signatures only hold scalar kinds here.
                self.take(scalar_size(kind).unwrap_or(0))?;
            }
        }
        Ok(())
    }

    /// Skips a field list: a count, then `(name hash, length, bytes)` per field.
    pub(crate) fn skip_fields(&mut self) -> Result<(), Error> {
        let count = self.u32()? as usize;
        if count.saturating_mul(8) > self.remaining() {
            return Err(Error::Truncated);
        }
        for _ in 0..count {
            self.u32()?;
            let len = self.u32()? as usize;
            self.take(len)?;
        }
        Ok(())
    }
}

/// Whether a value written with `sig` can be read into a `info`. Numbers convert between any
/// numeric kinds (range-checked when read), a value can become `Option` and back, and arrays and
/// lists interchange. Structs and enums always match here; their fields are matched one by one.
pub(crate) fn compatible(info: &TypeInfo, sig: Sig<'_>) -> bool {
    match (&info.kind, sig.kind()) {
        (TypeKind::Option(o), OPTION) => compatible((o.some)(), sig.element()),
        (TypeKind::Option(o), _) => compatible((o.some)(), sig),
        (_, OPTION) => compatible(info, sig.element()),
        (TypeKind::Primitive(p), kind) => {
            let own = primitive_kind(*p);
            own == kind || (p.is_numeric() && is_numeric(kind))
        }
        (TypeKind::String(_), STRING)
        | (TypeKind::Struct(_), STRUCT)
        | (TypeKind::Enum(_), ENUM) => true,
        (TypeKind::List(l), LIST | ARRAY) => compatible((l.element)(), sig.element()),
        (TypeKind::Array(a), LIST | ARRAY) => compatible((a.element)(), sig.element()),
        _ => false,
    }
}
