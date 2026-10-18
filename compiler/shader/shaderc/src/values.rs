//! Compile-time values between the IR interpreter's form (bits and aggregates, no types) and
//! the reflection's ([`ConstValue`]: typed, fields named), by the front-end type.

use marlin_front_hir_def::{ItemKind, ItemTreeQuery};
use marlin_lang_query::{Cancelled, Cx};
use marlin_shader::reflect::{ConstValue, FieldValue};
use marlin_shaderc_hir_ty::{
    BuiltinKind, Builtins, GenericArgTy, ItemTypesQuery, Scalar, Ty, fields_of,
};
use marlin_shaderc_ir::Val;
use marlin_shaderc_ir::num::{f16_to_f32, f64_to_f16};

/// What a value of a front-end type is made of.
enum Shape {
    Scalar(Scalar),
    /// Vectors and matrices: aggregates of aggregates of this scalar.
    Nested(Scalar),
    List(Vec<Ty>),
    /// An array: elements of one type.
    Array(Ty),
    Struct(Vec<(String, Ty)>),
}

fn shape(cx: &Cx<'_>, builtins: &Builtins, ty: &Ty) -> Result<Option<Shape>, Cancelled> {
    Ok(Some(match ty {
        Ty::Adt { item, args } => match builtins.kind(*item) {
            Some(BuiltinKind::Scalar(s)) => Shape::Scalar(s),
            Some(BuiltinKind::Vector | BuiltinKind::Matrix) => match args.first() {
                Some(GenericArgTy::Type(t)) => match shape(cx, builtins, t)? {
                    Some(Shape::Scalar(s)) => Shape::Nested(s),
                    _ => return Ok(None),
                },
                _ => return Ok(None),
            },
            Some(_) => return Ok(None),
            None => {
                let tree = cx.get::<ItemTreeQuery>(&item.file)?;
                match tree.items[item.local].kind {
                    // An enum is its underlying type (int by default).
                    ItemKind::Enum => {
                        let types = cx.get::<ItemTypesQuery>(item)?;
                        match types.bases.first() {
                            Some(t) => return shape(cx, builtins, t),
                            None => Shape::Scalar(Scalar::I32),
                        }
                    }
                    ItemKind::Struct => match fields_of(cx, builtins, *item, args)? {
                        Some(fields) => Shape::Struct(
                            fields
                                .into_iter()
                                .map(|f| (f.name.to_string(), f.ty))
                                .collect(),
                        ),
                        None => return Ok(None),
                    },
                    _ => return Ok(None),
                }
            }
        },
        Ty::Array { element, .. } => Shape::Array((**element).clone()),
        Ty::Tuple(items) => Shape::List(items.to_vec()),
        _ => return Ok(None),
    }))
}

/// `v` (of type `ty`) for the reflection; `None` for what has no compile-time value.
pub fn to_const(
    cx: &Cx<'_>,
    builtins: &Builtins,
    ty: &Ty,
    v: &Val,
) -> Result<Option<ConstValue>, Cancelled> {
    let Some(shape) = shape(cx, builtins, ty)? else {
        return Ok(None);
    };
    Ok(match (shape, v) {
        (Shape::Scalar(s), Val::Bits(b)) => Some(scalar_const(s, *b)),
        (Shape::Nested(s), v) => nested_const(s, v),
        (Shape::Array(t), Val::Agg(items)) => {
            let mut out = Vec::new();
            for item in items {
                let Some(c) = to_const(cx, builtins, &t, item)? else {
                    return Ok(None);
                };
                out.push(c);
            }
            Some(ConstValue::List(out))
        }
        (Shape::List(tys), Val::Agg(items)) if tys.len() == items.len() => {
            let mut out = Vec::new();
            for (t, item) in tys.iter().zip(items) {
                let Some(c) = to_const(cx, builtins, t, item)? else {
                    return Ok(None);
                };
                out.push(c);
            }
            Some(ConstValue::List(out))
        }
        (Shape::Struct(fields), Val::Agg(items)) if fields.len() == items.len() => {
            let mut out = Vec::new();
            for ((name, t), item) in fields.iter().zip(items) {
                let Some(value) = to_const(cx, builtins, t, item)? else {
                    return Ok(None);
                };
                out.push(FieldValue {
                    name: name.clone(),
                    value,
                });
            }
            Some(ConstValue::Struct(out))
        }
        _ => None,
    })
}

fn nested_const(s: Scalar, v: &Val) -> Option<ConstValue> {
    Some(match v {
        Val::Bits(b) => scalar_const(s, *b),
        Val::Agg(items) => ConstValue::List(
            items
                .iter()
                .map(|i| nested_const(s, i))
                .collect::<Option<_>>()?,
        ),
        _ => return None,
    })
}

fn scalar_const(s: Scalar, b: u64) -> ConstValue {
    match s {
        Scalar::Bool => ConstValue::Bool(b != 0),
        Scalar::I16 => ConstValue::Int(b as u16 as i16 as i64),
        Scalar::I32 => ConstValue::Int(b as u32 as i32 as i64),
        Scalar::I64 => ConstValue::Int(b as i64),
        Scalar::U16 | Scalar::U32 | Scalar::U64 => ConstValue::UInt(b),
        Scalar::F16 => ConstValue::Float(f16_to_f32(b as u16) as f64),
        Scalar::F32 => ConstValue::Float(f32::from_bits(b as u32) as f64),
        Scalar::F64 => ConstValue::Float(f64::from_bits(b)),
    }
}

/// `c` as a value of type `ty`; `None` when it does not fit (another shape, out of range).
pub fn from_const(
    cx: &Cx<'_>,
    builtins: &Builtins,
    ty: &Ty,
    c: &ConstValue,
) -> Result<Option<Val>, Cancelled> {
    let Some(shape) = shape(cx, builtins, ty)? else {
        return Ok(None);
    };
    Ok(match (shape, c) {
        (Shape::Scalar(s), c) => scalar_val(s, c),
        (Shape::Nested(s), c) => nested_val(s, c),
        (Shape::Array(t), ConstValue::List(items)) => {
            let mut out = Vec::new();
            for item in items {
                let Some(v) = from_const(cx, builtins, &t, item)? else {
                    return Ok(None);
                };
                out.push(v);
            }
            Some(Val::Agg(out))
        }
        (Shape::List(tys), ConstValue::List(items)) if tys.len() == items.len() => {
            let mut out = Vec::new();
            for (t, item) in tys.iter().zip(items) {
                let Some(v) = from_const(cx, builtins, t, item)? else {
                    return Ok(None);
                };
                out.push(v);
            }
            Some(Val::Agg(out))
        }
        (Shape::Struct(fields), ConstValue::Struct(values)) if fields.len() == values.len() => {
            let mut out = Vec::new();
            for ((name, t), fv) in fields.iter().zip(values) {
                if &fv.name != name {
                    return Ok(None);
                }
                let Some(v) = from_const(cx, builtins, t, &fv.value)? else {
                    return Ok(None);
                };
                out.push(v);
            }
            Some(Val::Agg(out))
        }
        _ => None,
    })
}

fn nested_val(s: Scalar, c: &ConstValue) -> Option<Val> {
    match c {
        ConstValue::List(items) => Some(Val::Agg(
            items
                .iter()
                .map(|i| nested_val(s, i))
                .collect::<Option<_>>()?,
        )),
        c => scalar_val(s, c),
    }
}

fn scalar_val(s: Scalar, c: &ConstValue) -> Option<Val> {
    let bits = match (s, c) {
        (Scalar::Bool, ConstValue::Bool(b)) => *b as u64,
        (Scalar::I16, ConstValue::Int(i)) => i16::try_from(*i).ok()? as u16 as u64,
        (Scalar::I32, ConstValue::Int(i)) => i32::try_from(*i).ok()? as u32 as u64,
        (Scalar::I64, ConstValue::Int(i)) => *i as u64,
        (Scalar::U16, ConstValue::UInt(u)) => u16::try_from(*u).ok()? as u64,
        (Scalar::U32, ConstValue::UInt(u)) => u32::try_from(*u).ok()? as u64,
        (Scalar::U64, ConstValue::UInt(u)) => *u,
        (Scalar::F16, ConstValue::Float(x)) => f64_to_f16(*x) as u64,
        (Scalar::F32, ConstValue::Float(x)) => (*x as f32).to_bits() as u64,
        (Scalar::F64, ConstValue::Float(x)) => x.to_bits(),
        _ => return None,
    };
    Some(Val::Bits(bits))
}
