//! Types: what each type id is, shape rules, no duplicate non-aggregate types, and Vulkan's
//! layout rules (scalar layout where the API lays memory out, no layout elsewhere).

use std::collections::{BTreeMap, HashSet};

use super::{Inst, Validator};
use crate::spirv::{decoration, op, storage_class as sc};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Ty {
    Void,
    Bool,
    Int {
        width: u32,
        signed: bool,
    },
    Float {
        width: u32,
    },
    Vector {
        component: u32,
        count: u32,
    },
    Matrix {
        column: u32,
        count: u32,
    },
    Array {
        element: u32,
        length: u32,
    },
    RuntimeArray {
        element: u32,
    },
    Struct(Vec<u32>),
    Pointer {
        class: u32,
        pointee: u32,
    },
    Untyped {
        class: u32,
    },
    Function {
        ret: u32,
        params: Vec<u32>,
    },
    /// The sampled type, dimension, depth, arrayed, multisampled, sampled and format words.
    Image(Vec<u32>),
    SampledImage(u32),
    RayQuery,
    AccelerationStructure,
    Sampler,
    Buffer {
        class: u32,
    },
}

#[derive(Default)]
pub(crate) struct Types {
    map: BTreeMap<u32, Ty>,
    /// Non-aggregate types already declared, to find duplicates.
    seen: HashSet<Ty>,
}

impl Types {
    pub fn get(&self, id: u32) -> Option<&Ty> {
        self.map.get(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&u32, &Ty)> {
        self.map.iter()
    }

    /// The scalar component of a scalar or vector type.
    pub fn component(&self, id: u32) -> Option<(u32, &Ty)> {
        match self.get(id)? {
            Ty::Vector { component, count } => Some((*count, self.get(*component)?)),
            t @ (Ty::Int { .. } | Ty::Float { .. } | Ty::Bool) => Some((1, t)),
            _ => None,
        }
    }
}

/// Records the type `i` declares (if it declares one), checking its shape.
pub(crate) fn declare(v: &mut Validator, i: &Inst) {
    let Some(r) = i.result else { return };
    if let Some(&k) = v.defs.get(&r) {
        let _ = k;
    }
    v.defs.entry(r).or_insert(v.insts.len() - 1);
    let o = &i.ops;
    let ty = match i.opcode {
        op::TypeVoid => Ty::Void,
        op::TypeBool => Ty::Bool,
        op::TypeInt => {
            if !matches!(o[1], 8 | 16 | 32 | 64) || o[2] > 1 {
                v.error(format!(
                    "{}: integers are 8, 16, 32 or 64 bits, signed 0 or 1",
                    i.name()
                ));
            }
            Ty::Int {
                width: o[1],
                signed: o[2] == 1,
            }
        }
        op::TypeFloat => {
            if !matches!(o[1], 16 | 32 | 64) {
                v.error(format!("{}: floats are 16, 32 or 64 bits", i.name()));
            }
            Ty::Float { width: o[1] }
        }
        op::TypeVector => {
            if !matches!(
                v.types.get(o[1]),
                Some(Ty::Int { .. } | Ty::Float { .. } | Ty::Bool)
            ) {
                v.error(format!("{}: a vector's components are scalars", i.name()));
            }
            if !(2..=4).contains(&o[2]) {
                v.error(format!("{}: vectors have 2 to 4 components here", i.name()));
            }
            Ty::Vector {
                component: o[1],
                count: o[2],
            }
        }
        op::TypeMatrix => {
            let float_columns = matches!(
                v.types.component(o[1]),
                Some((n, Ty::Float { .. })) if n >= 2
            );
            if !float_columns || !(2..=4).contains(&o[2]) {
                v.error(format!(
                    "{}: a matrix has 2 to 4 float vector columns",
                    i.name()
                ));
            }
            Ty::Matrix {
                column: o[1],
                count: o[2],
            }
        }
        op::TypeArray => {
            let length = constant_value(v, o[2]);
            if length.is_none_or(|n| n == 0) {
                v.error(format!(
                    "{}: an array's length is a positive integer constant",
                    i.name()
                ));
            }
            Ty::Array {
                element: o[1],
                length: length.unwrap_or(0),
            }
        }
        op::TypeRuntimeArray => Ty::RuntimeArray { element: o[1] },
        op::TypeStruct => Ty::Struct(o[1..].to_vec()),
        op::TypePointer => Ty::Pointer {
            class: o[1],
            pointee: o[2],
        },
        op::TypeUntypedPointerKHR => Ty::Untyped { class: o[1] },
        op::TypeFunction => Ty::Function {
            ret: o[1],
            params: o[2..].to_vec(),
        },
        op::TypeImage => Ty::Image(o[1..].to_vec()),
        op::TypeSampledImage => Ty::SampledImage(o[1]),
        op::TypeRayQueryKHR => Ty::RayQuery,
        op::TypeAccelerationStructureKHR => Ty::AccelerationStructure,
        op::TypeSampler => Ty::Sampler,
        op::TypeBufferEXT => {
            if !matches!(o[1], sc::StorageBuffer | sc::Uniform) {
                v.error(format!(
                    "{}: a buffer descriptor is `StorageBuffer` or `Uniform`",
                    i.name()
                ));
            }
            Ty::Buffer { class: o[1] }
        }
        _ => return,
    };
    let aggregate = matches!(
        ty,
        Ty::Struct(_) | Ty::Array { .. } | Ty::RuntimeArray { .. }
    );
    if !aggregate && !v.types.seen.insert(ty.clone()) {
        v.error(format!("{}: the same type is declared twice", i.name()));
    }
    v.types.map.insert(r, ty);
}

/// The value of 32-bit integer constant `id`.
pub(crate) fn constant_value(v: &Validator, id: u32) -> Option<u32> {
    let &k = v.defs.get(&id)?;
    let i = &v.insts[k];
    (i.opcode == op::Constant)
        .then(|| i.ops.get(2).copied())
        .flatten()
}

/// Size and alignment in scalar layout (`None` for what has none: runtime arrays, opaque
/// types, pointers).
pub(crate) fn scalar_layout(v: &Validator, t: u32) -> Option<(u32, u32)> {
    Some(match v.types.get(t)? {
        Ty::Bool => (4, 4),
        Ty::Int { width, .. } | Ty::Float { width } => (width / 8, width / 8),
        Ty::Vector { component, count } => {
            let (s, a) = scalar_layout(v, *component)?;
            (s * count, a)
        }
        Ty::Matrix { column, count } => {
            let (s, a) = scalar_layout(v, *column)?;
            (s * count, a)
        }
        Ty::Array { element, length } => {
            let stride = v
                .decorated(t, decoration::ArrayStride)
                .and_then(|e| e.first().copied());
            let (s, a) = scalar_layout(v, *element)?;
            (stride.unwrap_or(s.div_ceil(a) * a) * length, a)
        }
        Ty::Struct(members) => {
            let mut end = 0u32;
            let mut align = 1u32;
            for (m, &mt) in members.iter().enumerate() {
                let (s, a) = scalar_layout(v, mt).unwrap_or((0, 1));
                let offset = v
                    .member_decorated(t, m as u32, decoration::Offset)
                    .and_then(|e| e.first().copied())
                    .unwrap_or(end.div_ceil(a) * a);
                end = end.max(offset + s);
                align = align.max(a);
            }
            (end.div_ceil(align) * align, align)
        }
        _ => return None,
    })
}

/// A type in memory the API lays out: offsets, strides, scalar layout.
pub(crate) fn check_laid_out(v: &mut Validator, t: u32) {
    let mut seen = HashSet::new();
    laid_out(v, t, &mut seen);
}

fn laid_out(v: &mut Validator, t: u32, seen: &mut HashSet<u32>) {
    if !seen.insert(t) {
        return;
    }
    match v.types.get(t).cloned() {
        Some(Ty::Struct(members)) => {
            // (offset, end, member) of the members laid out, to find overlaps in offset order:
            // members need not come in it.
            let mut spans: Vec<(u32, u32, u32)> = Vec::new();
            for (m, mt) in members.iter().enumerate() {
                let m = m as u32;
                let Some(offset) = v
                    .member_decorated(t, m, decoration::Offset)
                    .and_then(|e| e.first().copied())
                else {
                    v.error(format!(
                        "%{t}: member {m} of a struct in laid-out memory has no `Offset`"
                    ));
                    continue;
                };
                if let Some((size, align)) = scalar_layout(v, *mt) {
                    if offset % align != 0 {
                        v.error(format!(
                            "%{t}: member {m}'s offset {offset} is not a multiple of its alignment {align}"
                        ));
                    }
                    spans.push((offset, offset.saturating_add(size), m));
                }
                if is_matrix(v, *mt) && v.member_decorated(t, m, decoration::MatrixStride).is_none()
                {
                    v.error(format!("%{t}: matrix member {m} has no `MatrixStride`"));
                }
                if matches!(v.types.get(*mt), Some(Ty::RuntimeArray { .. }))
                    && m as usize + 1 != members.len()
                {
                    v.error(format!("%{t}: a runtime array is the last member"));
                }
                laid_out(v, *mt, seen);
            }
            spans.sort_unstable();
            for w in spans.windows(2) {
                let ((_, end, a), (offset, _, b)) = (w[0], w[1]);
                if offset < end {
                    v.error(format!("%{t}: member {b} at {offset} overlaps member {a}"));
                }
            }
        }
        Some(Ty::Array { element, .. } | Ty::RuntimeArray { element }) => {
            let descriptors = matches!(
                v.types.get(element),
                Some(Ty::Buffer { .. } | Ty::Image(_) | Ty::Sampler | Ty::AccelerationStructure)
            );
            match v
                .decorated(t, decoration::ArrayStride)
                .and_then(|e| e.first().copied())
            {
                Some(stride) => {
                    if let Some((size, align)) = scalar_layout(v, element)
                        && (stride < size || stride % align != 0)
                    {
                        v.error(format!(
                            "%{t}: array stride {stride} is under the element's size {size} or not a multiple of {align}"
                        ));
                    }
                }
                None if descriptors => {}
                None => v.error(format!(
                    "%{t}: an array in laid-out memory has no `ArrayStride`"
                )),
            }
            laid_out(v, element, seen);
        }
        _ => {}
    }
}

fn is_matrix(v: &Validator, mut t: u32) -> bool {
    while let Some(Ty::Array { element, .. } | Ty::RuntimeArray { element }) = v.types.get(t) {
        t = *element;
    }
    matches!(v.types.get(t), Some(Ty::Matrix { .. }))
}

/// A type in memory the API does not lay out: no offsets or strides (Vulkan).
pub(crate) fn check_plain(v: &mut Validator, t: u32, class: u32) {
    let mut seen = HashSet::new();
    plain(v, t, class, &mut seen);
}

fn plain(v: &mut Validator, t: u32, class: u32, seen: &mut HashSet<u32>) {
    if !seen.insert(t) {
        return;
    }
    let name = sc::name(class).unwrap_or("?");
    match v.types.get(t).cloned() {
        Some(Ty::Struct(members)) => {
            for (m, mt) in members.iter().enumerate() {
                let decorated = [decoration::Offset, decoration::MatrixStride]
                    .iter()
                    .any(|&d| v.member_decorated(t, m as u32, d).is_some());
                if decorated {
                    v.error(format!(
                        "%{t}: a struct in `{name}` memory has explicit layout decorations (Vulkan)"
                    ));
                }
                plain(v, *mt, class, seen);
            }
        }
        Some(Ty::Array { element, .. } | Ty::RuntimeArray { element }) => {
            if v.decorated(t, decoration::ArrayStride).is_some() {
                v.error(format!(
                    "%{t}: an array in `{name}` memory has an `ArrayStride` (Vulkan)"
                ));
            }
            plain(v, element, class, seen);
        }
        _ => {}
    }
}
