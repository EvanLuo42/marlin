//! Decoding. Every read is bounds-checked and every count is checked against the remaining input
//! before anything is allocated, so corrupt input fails with an error instead of reading out of
//! bounds or exhausting memory.

use marlin_reflect::{Mut, Primitive, Reflect, TypeKind};

use crate::Error;
use crate::wire::{
    self, ARRAY, BOOL, CHAR, ENUM, F32, F64, I8, I16, I32, I64, I128, Input, LIST, OPTION, STRING,
    STRUCT, Sig, U8, U16, U32, U64, U128,
};

enum Scalar {
    Bool(bool),
    Char(char),
    U(u128),
    I(i128),
    F(f64),
}

fn read_scalar(input: &mut Input<'_>, kind: u8) -> Result<Scalar, Error> {
    Ok(match kind {
        BOOL => match input.u8()? {
            0 => Scalar::Bool(false),
            1 => Scalar::Bool(true),
            _ => return Err(Error::InvalidBool),
        },
        CHAR => Scalar::Char(char::from_u32(input.u32()?).ok_or(Error::InvalidChar)?),
        U8 => Scalar::U(input.u8()?.into()),
        U16 => Scalar::U(input.u16()?.into()),
        U32 => Scalar::U(input.u32()?.into()),
        U64 => Scalar::U(input.u64()?.into()),
        U128 => Scalar::U(u128::from_le_bytes(input.array()?)),
        I8 => Scalar::I(i8::from_le_bytes(input.array()?).into()),
        I16 => Scalar::I(i16::from_le_bytes(input.array()?).into()),
        I32 => Scalar::I(i32::from_le_bytes(input.array()?).into()),
        I64 => Scalar::I(i64::from_le_bytes(input.array()?).into()),
        I128 => Scalar::I(i128::from_le_bytes(input.array()?)),
        F32 => Scalar::F(f32::from_le_bytes(input.array()?).into()),
        F64 => Scalar::F(f64::from_le_bytes(input.array()?)),
        _ => return Err(Error::Incompatible),
    })
}

/// A float that holds an integer, as an integer.
fn float_to_int(f: f64) -> Option<i128> {
    // 2^127, exactly representable; `as` saturates outside the range.
    const LIMIT: f64 = 170_141_183_460_469_231_731_687_303_715_884_105_728.0;
    let i = f as i128;
    (f > -LIMIT && f < LIMIT && i as f64 == f).then_some(i)
}

fn set<T: Reflect>(dst: &mut Mut<'_>, value: Option<T>) -> Result<bool, Error> {
    match value {
        Some(value) => dst
            .set(value)
            .map(|()| true)
            .map_err(|_| Error::Incompatible),
        None => Ok(false),
    }
}

/// Stores `scalar` converted to `p`. Returns false if the value does not fit.
fn store(dst: &mut Mut<'_>, p: Primitive, scalar: Scalar) -> Result<bool, Error> {
    macro_rules! int {
        ($ty:ty) => {{
            let value = match scalar {
                Scalar::U(v) => <$ty>::try_from(v).ok(),
                Scalar::I(v) => <$ty>::try_from(v).ok(),
                Scalar::F(f) => float_to_int(f).and_then(|v| <$ty>::try_from(v).ok()),
                Scalar::Bool(_) | Scalar::Char(_) => return Err(Error::Incompatible),
            };
            set::<$ty>(dst, value)
        }};
    }
    macro_rules! float {
        ($ty:ty) => {{
            let value = match scalar {
                Scalar::U(v) => v as $ty,
                Scalar::I(v) => v as $ty,
                Scalar::F(f) => f as $ty,
                Scalar::Bool(_) | Scalar::Char(_) => return Err(Error::Incompatible),
            };
            set::<$ty>(dst, Some(value))
        }};
    }
    match p {
        Primitive::Bool => match scalar {
            Scalar::Bool(v) => set(dst, Some(v)),
            _ => Err(Error::Incompatible),
        },
        Primitive::Char => match scalar {
            Scalar::Char(v) => set(dst, Some(v)),
            _ => Err(Error::Incompatible),
        },
        Primitive::U8 => int!(u8),
        Primitive::U16 => int!(u16),
        Primitive::U32 => int!(u32),
        Primitive::U64 => int!(u64),
        Primitive::U128 => int!(u128),
        Primitive::Usize => int!(usize),
        Primitive::I8 => int!(i8),
        Primitive::I16 => int!(i16),
        Primitive::I32 => int!(i32),
        Primitive::I64 => int!(i64),
        Primitive::I128 => int!(i128),
        Primitive::Isize => int!(isize),
        Primitive::F32 => float!(f32),
        Primitive::F64 => float!(f64),
    }
}

pub(crate) struct Reader {
    pub(crate) max_depth: u32,
}

impl Reader {
    /// Reads a value written with `sig` into `dst`, which must be [`wire::compatible`] with it.
    /// `depth` is the remaining nesting budget.
    pub(crate) fn decode(
        &self,
        input: &mut Input<'_>,
        mut dst: Mut<'_>,
        sig: Sig<'_>,
        depth: u32,
    ) -> Result<(), Error> {
        let Some(depth) = depth.checked_sub(1) else {
            return Err(Error::TooDeep);
        };
        let info = dst.info();
        // The order of these arms must match `wire::compatible`.
        match (&info.kind, sig.kind()) {
            (TypeKind::Option(_), OPTION) => match input.u8()? {
                0 => {
                    dst.set_none();
                }
                1 => self.decode_some(input, dst, sig.element(), depth)?,
                _ => return Err(Error::InvalidTag),
            },
            (TypeKind::Option(_), _) => self.decode_some(input, dst, sig, depth)?,
            (_, OPTION) => match input.u8()? {
                // A field that stopped being optional keeps its default when it was `None`.
                0 => {}
                1 => self.decode(input, dst, sig.element(), depth)?,
                _ => return Err(Error::InvalidTag),
            },
            (TypeKind::Primitive(p), kind) => {
                let scalar = read_scalar(input, kind)?;
                // A number that no longer fits (say, a field narrowed to u8) leaves the value as
                // it was (the default, when loading) instead of failing the whole read.
                store(&mut dst, *p, scalar)?;
            }
            (TypeKind::String(_), STRING) => {
                let len = input.u32()? as usize;
                let bytes = input.take(len)?;
                let s = core::str::from_utf8(bytes).map_err(|_| Error::InvalidUtf8)?;
                dst.set_str(s);
            }
            (TypeKind::Struct(_), STRUCT) => self.decode_fields(input, dst, depth)?,
            (TypeKind::Enum(_), ENUM) => {
                let hash = input.u32()?;
                match info.variants().iter().position(|v| v.name_hash == hash) {
                    // A removed or renamed variant: skip its data and leave the value as it was.
                    None => input.skip_fields()?,
                    Some(index) => {
                        let current = dst.as_ref().variant().map(|(i, _)| i);
                        if current != Some(index) && !dst.set_variant(index) {
                            return Err(Error::NoDefault);
                        }
                        self.decode_fields(input, dst, depth)?;
                    }
                }
            }
            (TypeKind::List(_), LIST | ARRAY) => {
                let element = sig.element();
                let count = self.count(input, sig)?;
                // Rebuild from defaults so stale items never leak into the result.
                if !(dst.resize(0) && dst.resize(count)) {
                    return Err(Error::NoDefault);
                }
                for i in 0..count {
                    let item = dst.reborrow().index(i).ok_or(Error::Incompatible)?;
                    self.decode(input, item, element, depth)?;
                }
            }
            (TypeKind::Array(a), LIST | ARRAY) => {
                let element = sig.element();
                // Extra elements are dropped; missing ones keep their current value.
                for i in 0..self.count(input, sig)? {
                    if i < a.len {
                        let item = dst.reborrow().index(i).ok_or(Error::Incompatible)?;
                        self.decode(input, item, element, depth)?;
                    } else {
                        input.skip_value(element, depth)?;
                    }
                }
            }
            _ => return Err(Error::Incompatible),
        }
        Ok(())
    }

    fn count(&self, input: &mut Input<'_>, sig: Sig<'_>) -> Result<usize, Error> {
        let element = sig.element();
        if sig.kind() == LIST {
            return input.count(element);
        }
        let len = sig.array_len() as usize;
        input.check_count(len, element)?;
        Ok(len)
    }

    fn decode_some(
        &self,
        input: &mut Input<'_>,
        mut dst: Mut<'_>,
        sig: Sig<'_>,
        depth: u32,
    ) -> Result<(), Error> {
        if matches!(dst.as_ref().option(), Some(None)) && !dst.set_some_default() {
            return Err(Error::NoDefault);
        }
        let inner = dst.option().flatten().ok_or(Error::Incompatible)?;
        self.decode(input, inner, sig, depth)
    }

    /// Fields of a struct or of the active variant, matched by name hash. Unknown and transient
    /// fields are skipped; fields missing from the input keep their value.
    fn decode_fields(
        &self,
        input: &mut Input<'_>,
        mut dst: Mut<'_>,
        depth: u32,
    ) -> Result<(), Error> {
        let count = input.u32()? as usize;
        if count.saturating_mul(8) > input.remaining() {
            return Err(Error::Truncated);
        }
        for _ in 0..count {
            let hash = input.u32()?;
            let len = input.u32()? as usize;
            let mut body = Input::new(input.take(len)?);
            let fields = dst.as_ref().fields_info();
            let Some(index) = fields
                .iter()
                .position(|f| f.name_hash == hash && !f.attrs.transient)
            else {
                continue;
            };
            let sig = body.sig(self.max_depth)?;
            let (_, mut field) = dst.reborrow().field_at(index).ok_or(Error::Incompatible)?;
            // A field whose type changed beyond conversion keeps its value.
            if !wire::compatible(field.info(), sig) {
                continue;
            }
            match self.decode(&mut body, field.reborrow(), sig, depth) {
                Ok(()) if body.remaining() == 0 => {}
                Ok(()) => return Err(Error::InvalidLength),
                // The field's own length lets the rest of the input be read normally.
                Err(e) if e.is_schema() => {
                    if !field.reset() {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}
