//! Values, zero values, constants, and memory: logical memory as value trees (variables,
//! groupshared, push, entry I/O, constant data) and buffers as bytes in scalar layout (0004 §16).

use crate::interp::{ExecError, Val};
use crate::module::{Const, ConstValue, Module};
use crate::ty::{Scalar, Type, TypeData};

/// Arrays bigger than this many elements are not made in logical memory.
const MAX_ELEMENTS: u64 = 1 << 24;

pub(crate) fn zero(m: &Module, t: Type) -> Result<Val, ExecError> {
    let mut budget = MAX_ELEMENTS;
    zero_in(m, t, &mut budget)
}

fn zero_in(m: &Module, t: Type, budget: &mut u64) -> Result<Val, ExecError> {
    let take = |budget: &mut u64, n: u64| {
        if n > *budget {
            Err(ExecError::new("a value too large for the interpreter"))
        } else {
            *budget -= n;
            Ok(())
        }
    };
    Ok(match m.data(t) {
        TypeData::Scalar(_) | TypeData::Res(_) => Val::Bits(0),
        TypeData::Vector(_, n) => {
            take(budget, *n as u64)?;
            Val::Agg(vec![Val::Bits(0); *n as usize])
        }
        TypeData::Matrix { rows, cols, .. } => {
            take(budget, (*rows as u64) * (*cols as u64))?;
            Val::Agg(vec![
                Val::Agg(vec![Val::Bits(0); *rows as usize]);
                *cols as usize
            ])
        }
        TypeData::Array(e, n) => {
            take(budget, *n as u64)?;
            let item = zero_in(m, *e, budget)?;
            Val::Agg(vec![item; *n as usize])
        }
        TypeData::Struct(s) => {
            let fields: Vec<Type> = m.structs[*s].fields.iter().map(|f| f.1).collect();
            let mut items = Vec::with_capacity(fields.len());
            for f in fields {
                items.push(zero_in(m, f, budget)?);
            }
            Val::Agg(items)
        }
        TypeData::Ptr { .. } | TypeData::Handle(_) | TypeData::RayQuery => {
            return Err(ExecError::new("this type has no stored values"));
        }
    })
}

/// The value of a constant; `any` is zero.
pub(crate) fn constant(m: &Module, c: Const) -> Result<Val, ExecError> {
    let data = &m.consts[c];
    Ok(match &data.value {
        ConstValue::Int(b) | ConstValue::Float(b) => Val::Bits(*b),
        ConstValue::Zero | ConstValue::Any => zero(m, data.ty)?,
        ConstValue::Composite(items) => {
            let mut out = Vec::with_capacity(items.len());
            for &i in items {
                out.push(constant(m, i)?);
            }
            Val::Agg(out)
        }
    })
}

// ---- scalar layout ----

#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub size: u64,
    pub align: u64,
}

pub(crate) fn round_up(x: u64, a: u64) -> u64 {
    x.div_ceil(a.max(1)) * a.max(1)
}

pub(crate) fn scalar_bytes(s: Scalar) -> u64 {
    match s {
        Scalar::I1 | Scalar::I32 | Scalar::F32 => 4,
        Scalar::I16 | Scalar::F16 => 2,
        Scalar::I64 | Scalar::F64 => 8,
    }
}

pub(crate) fn layout(m: &Module, t: Type) -> Result<Layout, ExecError> {
    Ok(match m.data(t) {
        TypeData::Scalar(s) => Layout {
            size: scalar_bytes(*s),
            align: scalar_bytes(*s),
        },
        TypeData::Res(_) => Layout { size: 4, align: 4 },
        TypeData::Vector(s, n) => Layout {
            size: scalar_bytes(*s) * *n as u64,
            align: scalar_bytes(*s),
        },
        TypeData::Matrix {
            element,
            rows,
            cols,
        } => Layout {
            size: scalar_bytes(*element) * (*rows as u64) * (*cols as u64),
            align: scalar_bytes(*element),
        },
        TypeData::Array(e, n) => {
            let l = layout(m, *e)?;
            Layout {
                size: round_up(l.size, l.align).saturating_mul(*n as u64),
                align: l.align,
            }
        }
        TypeData::Struct(s) => {
            let mut offset = 0;
            let mut align = 1;
            for &(_, f) in &m.structs[*s].fields {
                let l = layout(m, f)?;
                offset = round_up(offset, l.align) + l.size;
                align = align.max(l.align);
            }
            Layout {
                size: round_up(offset, align),
                align,
            }
        }
        _ => return Err(ExecError::new("this type is not stored in buffers")),
    })
}

/// The byte offset of field `i` of struct type `t`.
pub(crate) fn field_offset(m: &Module, t: Type, i: u32) -> Result<(u64, Type), ExecError> {
    let TypeData::Struct(s) = m.data(t) else {
        return Err(ExecError::new("a field of something that is not a struct"));
    };
    let mut offset = 0;
    for (k, &(_, f)) in m.structs[*s].fields.iter().enumerate() {
        let l = layout(m, f)?;
        offset = round_up(offset, l.align);
        if k as u32 == i {
            return Ok((offset, f));
        }
        offset += l.size;
    }
    Err(ExecError::new("no such field"))
}

/// The stride and type of the elements of an array, vector or matrix (columns).
pub(crate) fn element(m: &Module, t: Type) -> Result<(u64, Type), ExecError> {
    let e = crate::infer::step_type_in(
        &mut crate::infer::Existing(m),
        t,
        crate::op::AccessStep::Index,
    )
    .map_err(ExecError::new)?;
    let l = layout(m, e)?;
    Ok((round_up(l.size, l.align), e))
}

/// Reads a value of type `t` at `offset`; `None` when it does not fit in `bytes`.
pub(crate) fn read(
    m: &Module,
    bytes: &[u8],
    offset: u64,
    t: Type,
) -> Result<Option<Val>, ExecError> {
    let size = layout(m, t)?.size;
    if offset
        .checked_add(size)
        .is_none_or(|end| end > bytes.len() as u64)
    {
        return Ok(None);
    }
    Ok(Some(read_at(m, bytes, offset as usize, t)?))
}

fn read_at(m: &Module, bytes: &[u8], at: usize, t: Type) -> Result<Val, ExecError> {
    let scalar = |s: Scalar, at: usize| -> Val {
        let n = scalar_bytes(s) as usize;
        let mut b = [0u8; 8];
        b[..n].copy_from_slice(&bytes[at..at + n]);
        let v = u64::from_le_bytes(b);
        Val::Bits(if s == Scalar::I1 { (v != 0) as u64 } else { v })
    };
    Ok(match m.data(t) {
        TypeData::Scalar(s) => scalar(*s, at),
        TypeData::Res(_) => scalar(Scalar::I32, at),
        TypeData::Vector(s, n) => Val::Agg(
            (0..*n as usize)
                .map(|i| scalar(*s, at + i * scalar_bytes(*s) as usize))
                .collect(),
        ),
        TypeData::Matrix {
            element,
            rows,
            cols,
        } => {
            let e = scalar_bytes(*element) as usize;
            Val::Agg(
                (0..*cols as usize)
                    .map(|c| {
                        Val::Agg(
                            (0..*rows as usize)
                                .map(|r| scalar(*element, at + (c * *rows as usize + r) * e))
                                .collect(),
                        )
                    })
                    .collect(),
            )
        }
        TypeData::Array(e, n) => {
            let l = layout(m, *e)?;
            let stride = round_up(l.size, l.align) as usize;
            let mut out = Vec::with_capacity(*n as usize);
            for i in 0..*n as usize {
                out.push(read_at(m, bytes, at + i * stride, *e)?);
            }
            Val::Agg(out)
        }
        TypeData::Struct(s) => {
            let mut out = Vec::new();
            for i in 0..m.structs[*s].fields.len() {
                let (off, f) = field_offset(m, t, i as u32)?;
                out.push(read_at(m, bytes, at + off as usize, f)?);
            }
            Val::Agg(out)
        }
        _ => return Err(ExecError::new("this type is not stored in buffers")),
    })
}

/// Writes `v` of type `t` at `offset`; `false` when it does not fit in `bytes`.
pub(crate) fn write(
    m: &Module,
    bytes: &mut [u8],
    offset: u64,
    t: Type,
    v: &Val,
) -> Result<bool, ExecError> {
    let size = layout(m, t)?.size;
    if offset
        .checked_add(size)
        .is_none_or(|end| end > bytes.len() as u64)
    {
        return Ok(false);
    }
    write_at(m, bytes, offset as usize, t, v)?;
    Ok(true)
}

fn write_at(m: &Module, bytes: &mut [u8], at: usize, t: Type, v: &Val) -> Result<(), ExecError> {
    let bad = || ExecError::new("a stored value does not have its type's shape");
    let scalar = |bytes: &mut [u8], s: Scalar, at: usize, v: &Val| -> Result<(), ExecError> {
        let Val::Bits(b) = v else { return Err(bad()) };
        let n = scalar_bytes(s) as usize;
        bytes[at..at + n].copy_from_slice(&b.to_le_bytes()[..n]);
        Ok(())
    };
    match (m.data(t), v) {
        (TypeData::Scalar(s), _) => scalar(bytes, *s, at, v)?,
        (TypeData::Res(_), _) => scalar(bytes, Scalar::I32, at, v)?,
        (TypeData::Vector(s, _), Val::Agg(items)) => {
            for (i, item) in items.iter().enumerate() {
                scalar(bytes, *s, at + i * scalar_bytes(*s) as usize, item)?;
            }
        }
        (TypeData::Matrix { element, rows, .. }, Val::Agg(cols)) => {
            let e = scalar_bytes(*element) as usize;
            for (c, col) in cols.iter().enumerate() {
                let Val::Agg(items) = col else {
                    return Err(bad());
                };
                for (r, item) in items.iter().enumerate() {
                    scalar(bytes, *element, at + (c * *rows as usize + r) * e, item)?;
                }
            }
        }
        (TypeData::Array(e, _), Val::Agg(items)) => {
            let l = layout(m, *e)?;
            let stride = round_up(l.size, l.align) as usize;
            for (i, item) in items.iter().enumerate() {
                write_at(m, bytes, at + i * stride, *e, item)?;
            }
        }
        (TypeData::Struct(_), Val::Agg(items)) => {
            for (i, item) in items.iter().enumerate() {
                let (off, f) = field_offset(m, t, i as u32)?;
                write_at(m, bytes, at + off as usize, f, item)?;
            }
        }
        _ => return Err(bad()),
    }
    Ok(())
}

/// The part of `root` at `path`; `None` when an index is out of range.
pub(crate) fn get_path<'v>(root: &'v Val, path: &[u64]) -> Option<&'v Val> {
    let mut v = root;
    for &i in path {
        match v {
            Val::Agg(items) => v = items.get(usize::try_from(i).ok()?)?,
            _ => return None,
        }
    }
    Some(v)
}

pub(crate) fn get_path_mut<'v>(root: &'v mut Val, path: &[u64]) -> Option<&'v mut Val> {
    let mut v = root;
    for &i in path {
        match v {
            Val::Agg(items) => v = items.get_mut(usize::try_from(i).ok()?)?,
            _ => return None,
        }
    }
    Some(v)
}
