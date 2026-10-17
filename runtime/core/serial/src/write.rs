//! Encoding.

use alloc::vec::Vec;

use marlin_reflect::{FieldInfo, Primitive, Ref, TypeInfo, TypeKind};

use crate::Error;
use crate::custom;
use crate::wire::{
    self, ARRAY, CUSTOM, ENUM, LIST, MAP, OPTION, SET, STRING, STRUCT, TUPLE, VERSIONED,
};

pub(crate) fn write_sig(out: &mut Vec<u8>, info: &'static TypeInfo) -> Result<(), Error> {
    let version = info.attrs.version;
    match &info.kind {
        TypeKind::Primitive(p) => out.push(wire::primitive_kind(*p)),
        TypeKind::String(_) => out.push(STRING),
        TypeKind::Struct(_) | TypeKind::Enum(_) => {
            if version > 0 {
                out.push(VERSIONED);
                out.extend_from_slice(&version.to_le_bytes());
            }
            out.push(if matches!(info.kind, TypeKind::Enum(_)) {
                ENUM
            } else {
                STRUCT
            });
        }
        TypeKind::Tuple(_) => out.push(TUPLE),
        TypeKind::List(l) => {
            out.push(LIST);
            write_sig(out, (l.element)())?;
        }
        TypeKind::Array(a) => {
            out.push(ARRAY);
            out.extend_from_slice(&len_u32(a.len)?.to_le_bytes());
            write_sig(out, (a.element)())?;
        }
        TypeKind::Option(o) => {
            out.push(OPTION);
            write_sig(out, (o.some)())?;
        }
        TypeKind::Map(m) => {
            out.push(MAP);
            write_sig(out, (m.key)())?;
            write_sig(out, (m.value)())?;
        }
        TypeKind::Set(s) => {
            out.push(SET);
            write_sig(out, (s.element)())?;
        }
        TypeKind::Pointer(p) => write_sig(out, (p.pointee)())?,
        TypeKind::Opaque => {
            if custom::serial_fns(info).is_some() {
                out.push(CUSTOM);
                out.extend_from_slice(&info.id.0.to_le_bytes());
            } else if let Some(proxy) = custom::proxy(info) {
                write_sig(out, (proxy.proxy)())?;
            } else {
                return Err(Error::Unsupported(info.id));
            }
        }
    }
    Ok(())
}

fn len_u32(len: usize) -> Result<u32, Error> {
    u32::try_from(len).map_err(|_| Error::TooLarge)
}

macro_rules! put {
    ($out:expr, $value:expr, $ty:ty, $info:expr) => {{
        let v: &$ty = $value
            .downcast::<$ty>()
            .ok_or(Error::Unsupported($info.id))?;
        $out.extend_from_slice(&v.to_le_bytes());
    }};
}

fn write_primitive(out: &mut Vec<u8>, value: Ref<'_>, p: Primitive) -> Result<(), Error> {
    let info = value.info();
    match p {
        Primitive::Bool => {
            let v = value
                .downcast::<bool>()
                .ok_or(Error::Unsupported(info.id))?;
            out.push(u8::from(*v));
        }
        Primitive::Char => {
            let v = value
                .downcast::<char>()
                .ok_or(Error::Unsupported(info.id))?;
            out.extend_from_slice(&u32::from(*v).to_le_bytes());
        }
        Primitive::U8 => put!(out, value, u8, info),
        Primitive::U16 => put!(out, value, u16, info),
        Primitive::U32 => put!(out, value, u32, info),
        Primitive::U64 => put!(out, value, u64, info),
        Primitive::U128 => put!(out, value, u128, info),
        Primitive::Usize => {
            let v = value
                .downcast::<usize>()
                .ok_or(Error::Unsupported(info.id))?;
            out.extend_from_slice(&(*v as u64).to_le_bytes());
        }
        Primitive::I8 => put!(out, value, i8, info),
        Primitive::I16 => put!(out, value, i16, info),
        Primitive::I32 => put!(out, value, i32, info),
        Primitive::I64 => put!(out, value, i64, info),
        Primitive::I128 => put!(out, value, i128, info),
        Primitive::Isize => {
            let v = value
                .downcast::<isize>()
                .ok_or(Error::Unsupported(info.id))?;
            out.extend_from_slice(&(*v as i64).to_le_bytes());
        }
        Primitive::F32 => put!(out, value, f32, info),
        Primitive::F64 => put!(out, value, f64, info),
    }
    Ok(())
}

pub(crate) fn write_value(out: &mut Vec<u8>, value: Ref<'_>) -> Result<(), Error> {
    let info = value.info();
    let unsupported = Error::Unsupported(info.id);
    match &info.kind {
        TypeKind::Primitive(p) => write_primitive(out, value, *p)?,
        TypeKind::String(_) => {
            let s = value.as_str().ok_or(unsupported)?;
            out.extend_from_slice(&len_u32(s.len())?.to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        TypeKind::Struct(_) | TypeKind::Tuple(_) => write_fields(out, value)?,
        TypeKind::Enum(_) => {
            let (_, variant) = value.variant().ok_or(unsupported)?;
            out.extend_from_slice(&variant.name_hash.to_le_bytes());
            write_fields(out, value)?;
        }
        TypeKind::List(_) | TypeKind::Array(_) => {
            let len = value.len().unwrap_or(0);
            if let TypeKind::List(_) = info.kind {
                out.extend_from_slice(&len_u32(len)?.to_le_bytes());
            }
            for i in 0..len {
                write_value(out, value.index(i).ok_or(unsupported)?)?;
            }
        }
        TypeKind::Option(_) => match value.option().flatten() {
            None => out.push(0),
            Some(inner) => {
                out.push(1);
                write_value(out, inner)?;
            }
        },
        TypeKind::Map(m) => {
            let entries = value.map_entries().ok_or(unsupported)?;
            out.extend_from_slice(&len_u32(entries.len())?.to_le_bytes());
            if m.ordered {
                for (k, v) in entries {
                    write_value(out, k)?;
                    write_value(out, v)?;
                }
            } else {
                // Hash map order depends on history: sort the encoded entries (key first) so
                // equal maps encode alike.
                let mut encoded = Vec::with_capacity(entries.len());
                for (k, v) in entries {
                    let mut entry = Vec::new();
                    write_value(&mut entry, k)?;
                    write_value(&mut entry, v)?;
                    encoded.push(entry);
                }
                encoded.sort_unstable();
                for entry in encoded {
                    out.extend_from_slice(&entry);
                }
            }
        }
        TypeKind::Set(s) => {
            let elements = value.set_elements().ok_or(unsupported)?;
            out.extend_from_slice(&len_u32(elements.len())?.to_le_bytes());
            if s.ordered {
                for e in elements {
                    write_value(out, e)?;
                }
            } else {
                let mut encoded = Vec::with_capacity(elements.len());
                for e in elements {
                    let mut bytes = Vec::new();
                    write_value(&mut bytes, e)?;
                    encoded.push(bytes);
                }
                encoded.sort_unstable();
                for bytes in encoded {
                    out.extend_from_slice(&bytes);
                }
            }
        }
        TypeKind::Pointer(_) => write_value(out, value.pointee().ok_or(unsupported)?)?,
        TypeKind::Opaque => {
            if let Some(fns) = custom::serial_fns(info) {
                let len_at = out.len();
                out.extend_from_slice(&[0; 4]);
                (fns.write)(value, out)?;
                let len = len_u32(out.len() - len_at - 4)?;
                out[len_at..len_at + 4].copy_from_slice(&len.to_le_bytes());
            } else if let Some(proxy) = custom::proxy(info) {
                let proxy = proxy.to_proxy(value).ok_or(unsupported)?;
                write_value(out, proxy.as_ref())?;
            } else {
                return Err(unsupported);
            }
        }
    }
    Ok(())
}

fn serialized(field: &FieldInfo) -> bool {
    !field.attrs.transient
}

/// Field count, then per field: name hash, byte length of what follows, signature, value.
fn write_fields(out: &mut Vec<u8>, value: Ref<'_>) -> Result<(), Error> {
    let count = value.fields_info().iter().filter(|f| serialized(f)).count();
    out.extend_from_slice(&len_u32(count)?.to_le_bytes());
    for (field, field_value) in value.fields() {
        if !serialized(field) {
            continue;
        }
        out.extend_from_slice(&field.name_hash.to_le_bytes());
        let len_at = out.len();
        out.extend_from_slice(&[0; 4]);
        write_sig(out, field_value.info())?;
        write_value(out, field_value)?;
        let len = len_u32(out.len() - len_at - 4)?;
        out[len_at..len_at + 4].copy_from_slice(&len.to_le_bytes());
    }
    Ok(())
}
