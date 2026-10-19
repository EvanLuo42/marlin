//! Decoding. Every read is bounds-checked and every count is checked against the remaining input
//! before anything is allocated, so corrupt input fails with an error instead of reading out of
//! bounds or exhausting memory.

use alloc::vec::Vec;

use marlin_reflect::{Mut, Primitive, Reflect, ReflectDynamic, TypeId, TypeKind, Value};

use crate::migrate::{self, Migrate, OldFields};
use crate::wire::{
    self, ARRAY, BOOL, CHAR, CUSTOM, ENUM, F32, F64, I8, I16, I32, I64, I128, Input, LIST, MAP,
    OPTION, SET, STRING, STRUCT, Sig, TUPLE, U8, U16, U32, U64, U128,
};
use crate::{Error, custom};

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

#[derive(Clone, Copy)]
pub(crate) struct Reader {
    pub(crate) max_depth: u32,
}

/// Fields of the data that no field of the target took, kept for a migration.
type Unmatched<'a> = Vec<(u32, &'a [u8])>;

impl Reader {
    /// Reads a value written with `sig` into `dst`, which must be [`wire::compatible`] with it.
    /// `depth` is the remaining nesting budget.
    pub(crate) fn decode<'a>(
        &self,
        input: &mut Input<'a>,
        mut dst: Mut<'_>,
        sig: Sig<'_>,
        depth: u32,
    ) -> Result<(), Error> {
        let Some(depth) = depth.checked_sub(1) else {
            return Err(Error::TooDeep);
        };
        let info = dst.info();
        // The order of these checks must match `wire::compatible`.
        match &info.kind {
            TypeKind::Pointer(_) => {
                // A shared `Rc`/`Arc` is unshared (cloned) first, like `Rc::make_mut`.
                let inner = dst.pointee_make_mut().ok_or(Error::Incompatible)?;
                return self.decode(input, inner, sig, depth);
            }
            TypeKind::Opaque => return self.decode_opaque(input, dst, sig, depth),
            _ => {}
        }
        let (version, plain) = sig.unversioned();
        match (&info.kind, plain.kind()) {
            (TypeKind::Option(_), OPTION) => match input.u8()? {
                0 => {
                    dst.set_none();
                }
                1 => self.decode_some(input, dst, plain.element(), depth)?,
                _ => return Err(Error::InvalidTag),
            },
            (TypeKind::Option(_), _) => self.decode_some(input, dst, sig, depth)?,
            (_, OPTION) => match input.u8()? {
                // A field that stopped being optional keeps its default when it was `None`.
                0 => {}
                1 => self.decode(input, dst, plain.element(), depth)?,
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
            (TypeKind::Struct(_) | TypeKind::Tuple(_), STRUCT | TUPLE)
            | (TypeKind::Enum(_), ENUM)
                if migrate::migration(info, version).is_some() =>
            {
                self.decode_migrated(input, dst, version, depth)?;
            }
            (TypeKind::Struct(_) | TypeKind::Tuple(_), STRUCT | TUPLE) => {
                self.decode_fields(input, dst, depth, None)?;
            }
            (TypeKind::Enum(_), ENUM) => {
                let hash = input.u32()?;
                match info.variants().iter().position(|v| v.name_hash == hash) {
                    // A removed or renamed variant: skip its data and leave the value as it was.
                    None => input.skip_fields()?,
                    Some(index) => {
                        select_variant(&mut dst, index)?;
                        self.decode_fields(input, dst, depth, None)?;
                    }
                }
            }
            (TypeKind::List(_), LIST | ARRAY | SET) => {
                let element = plain.element();
                let count = self.count(input, plain)?;
                // Rebuild from defaults so stale items never leak into the result.
                if !(dst.resize(0) && dst.resize(count)) {
                    return Err(Error::NoDefault);
                }
                for i in 0..count {
                    let item = dst.reborrow().index(i).ok_or(Error::Incompatible)?;
                    self.decode(input, item, element, depth)?;
                }
            }
            (TypeKind::Array(a), LIST | ARRAY | SET) => {
                let element = plain.element();
                // Extra elements are dropped; missing ones keep their current value.
                for i in 0..self.count(input, plain)? {
                    if i < a.len {
                        let item = dst.reborrow().index(i).ok_or(Error::Incompatible)?;
                        self.decode(input, item, element, depth)?;
                    } else {
                        input.skip_value(element, depth)?;
                    }
                }
            }
            (TypeKind::Set(_), LIST | ARRAY | SET) => self.decode_set(input, dst, plain, depth)?,
            (TypeKind::Map(_), MAP) => self.decode_map(input, dst, plain, depth)?,
            _ => return Err(Error::Incompatible),
        }
        Ok(())
    }

    // The helpers below are kept out of `decode`, which recurses once per nesting level: their
    // locals would otherwise grow every level's stack frame.

    #[inline(never)]
    fn decode_set(
        &self,
        input: &mut Input<'_>,
        mut dst: Mut<'_>,
        sig: Sig<'_>,
        depth: u32,
    ) -> Result<(), Error> {
        let TypeKind::Set(s) = &dst.info().kind else {
            return Err(Error::Incompatible);
        };
        let element_info = (s.element)();
        let element = sig.element();
        let count = self.count(input, sig)?;
        dst.clear();
        for _ in 0..count {
            let mut item = Value::default_of(element_info).ok_or(Error::NoDefault)?;
            self.decode(input, item.as_mut(), element, depth)?;
            dst.set_insert(item).map_err(|_| Error::Incompatible)?;
        }
        Ok(())
    }

    #[inline(never)]
    fn decode_map(
        &self,
        input: &mut Input<'_>,
        mut dst: Mut<'_>,
        sig: Sig<'_>,
        depth: u32,
    ) -> Result<(), Error> {
        let TypeKind::Map(m) = &dst.info().kind else {
            return Err(Error::Incompatible);
        };
        let (key_info, value_info) = ((m.key)(), (m.value)());
        let count = input.u32()? as usize;
        input.check_count(count, sig.entry_min_size())?;
        dst.clear();
        for _ in 0..count {
            let mut key = Value::default_of(key_info).ok_or(Error::NoDefault)?;
            self.decode(input, key.as_mut(), sig.element(), depth)?;
            let mut value = Value::default_of(value_info).ok_or(Error::NoDefault)?;
            self.decode(input, value.as_mut(), sig.map_value(), depth)?;
            dst.map_insert(key, value)
                .map_err(|_| Error::Incompatible)?;
        }
        Ok(())
    }

    /// A struct, tuple or enum of an older data version: fields as usual, collecting the
    /// unmatched ones, then the migration steps.
    #[inline(never)]
    fn decode_migrated(
        &self,
        input: &mut Input<'_>,
        mut dst: Mut<'_>,
        version: u32,
        depth: u32,
    ) -> Result<(), Error> {
        let info = dst.info();
        let migrate: &Migrate = migrate::migration(info, version).ok_or(Error::Incompatible)?;
        let mut fields = Vec::new();
        let mut variant = None;
        if let TypeKind::Enum(_) = info.kind {
            let hash = input.u32()?;
            match info.variants().iter().position(|v| v.name_hash == hash) {
                // A removed or renamed variant: the value keeps its variant, and the migration
                // gets the old variant's fields.
                None => {
                    fields = raw_fields(input)?;
                    variant = Some(hash);
                }
                Some(index) => {
                    select_variant(&mut dst, index)?;
                    self.decode_fields(input, dst.reborrow(), depth, Some(&mut fields))?;
                }
            }
        } else {
            self.decode_fields(input, dst.reborrow(), depth, Some(&mut fields))?;
        }
        let to = info.attrs.version;
        let old = OldFields::new(fields, variant, *self, depth);
        for from in version..to {
            (migrate.migrate)(from, dst.reborrow(), &old)?;
        }
        Ok(())
    }

    /// An opaque value, through its custom serializer or its proxy.
    #[inline(never)]
    fn decode_opaque(
        &self,
        input: &mut Input<'_>,
        dst: Mut<'_>,
        sig: Sig<'_>,
        depth: u32,
    ) -> Result<(), Error> {
        let info = dst.info();
        if sig.kind() == CUSTOM {
            let len = input.u32()? as usize;
            let bytes = input.take(len)?;
            if let Some(fns) = custom::serial_fns(info) {
                return (fns.read)(dst, bytes);
            }
            let dynamic = custom::dynamic(info).ok_or(Error::Incompatible)?;
            return self.decode_held(dynamic, dst, bytes, depth);
        }
        let proxy = custom::proxy(info).ok_or(Error::Incompatible)?;
        // Start from the current value, so a struct proxy keeps fields the data lacks.
        let mut value = match proxy.to_proxy(dst.as_ref()) {
            Some(value) => value,
            None => Value::default_of((proxy.proxy)()).ok_or(Error::NoDefault)?,
        };
        self.decode(input, value.as_mut(), sig, depth)?;
        if proxy.from_proxy(dst, value.as_ref()) {
            Ok(())
        } else {
            Err(Error::Incompatible)
        }
    }

    /// A [`ReflectDynamic`] holder's value: the held type's id, then its signature and value
    /// (see [`custom`]). An id of no registered type the holder accepts, or a value that no
    /// longer fits the type, leaves the holder as it was.
    fn decode_held(
        &self,
        dynamic: &ReflectDynamic,
        dst: Mut<'_>,
        bytes: &[u8],
        depth: u32,
    ) -> Result<(), Error> {
        let mut body = Input::new(bytes);
        let id = body.u64()?;
        if id == 0 {
            return Ok(());
        }
        let Some(held) = marlin_reflect::find(TypeId(id)).filter(|&i| dynamic.accepts(i)) else {
            return Ok(());
        };
        let sig = body.sig(self.max_depth)?;
        if !wire::compatible(held, sig) {
            return Ok(());
        }
        let mut value = Value::default_of(held).ok_or(Error::NoDefault)?;
        self.decode(&mut body, value.as_mut(), sig, depth)?;
        if body.remaining() != 0 {
            return Err(Error::InvalidLength);
        }
        dynamic.hold(dst, value).map_err(|_| Error::Incompatible)
    }

    fn count(&self, input: &mut Input<'_>, sig: Sig<'_>) -> Result<usize, Error> {
        let element = sig.element();
        if sig.kind() != ARRAY {
            return input.count(element);
        }
        let len = sig.array_len() as usize;
        input.check_count(len, element.min_size())?;
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

    /// Fields of a struct, tuple or the active variant, matched by name hash. Unknown and
    /// transient fields are skipped (and collected in `unmatched`, if given, along with fields
    /// whose type changed beyond conversion); fields missing from the input keep their value.
    fn decode_fields<'a>(
        &self,
        input: &mut Input<'a>,
        mut dst: Mut<'_>,
        depth: u32,
        mut unmatched: Option<&mut Unmatched<'a>>,
    ) -> Result<(), Error> {
        let count = input.u32()? as usize;
        if count.saturating_mul(8) > input.remaining() {
            return Err(Error::Truncated);
        }
        for _ in 0..count {
            let hash = input.u32()?;
            let len = input.u32()? as usize;
            let bytes = input.take(len)?;
            let mut body = Input::new(bytes);
            let fields = dst.as_ref().fields_info();
            let Some(index) = fields
                .iter()
                .position(|f| f.name_hash == hash && !f.attrs.transient)
            else {
                if let Some(unmatched) = unmatched.as_deref_mut() {
                    unmatched.push((hash, bytes));
                }
                continue;
            };
            let sig = body.sig(self.max_depth)?;
            let (_, mut field) = dst.reborrow().field_at(index).ok_or(Error::Incompatible)?;
            // A field whose type changed beyond conversion keeps its value.
            if !wire::compatible(field.info(), sig) {
                if let Some(unmatched) = unmatched.as_deref_mut() {
                    unmatched.push((hash, bytes));
                }
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

/// Switches an enum to variant `index` unless it is already active.
fn select_variant(dst: &mut Mut<'_>, index: usize) -> Result<(), Error> {
    let current = dst.as_ref().variant().map(|(i, _)| i);
    if current != Some(index) && !dst.set_variant(index) {
        return Err(Error::NoDefault);
    }
    Ok(())
}

/// The fields of a struct body, unparsed: `(name hash, body)` each.
fn raw_fields<'a>(input: &mut Input<'a>) -> Result<Unmatched<'a>, Error> {
    let count = input.u32()? as usize;
    if count.saturating_mul(8) > input.remaining() {
        return Err(Error::Truncated);
    }
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let hash = input.u32()?;
        let len = input.u32()? as usize;
        fields.push((hash, input.take(len)?));
    }
    Ok(fields)
}
