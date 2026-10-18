//! The result types of an operation from its operands' types. The builder and the text parser
//! use it, so most instructions never spell out their types; the few whose result cannot be
//! inferred (conversions, `splat`, `construct`, `var`) take it explicitly. It reports operands
//! it cannot make sense of; whether the operands are otherwise valid is the verifier's job.
//!
//! It runs over [`Types`]: a module that interns the types it needs (building, parsing) or,
//! for the verifier, one that only finds the types already there ([`Existing`]).

use crate::module::Module;
use crate::op::{AccessStep, MathOp, Op, WaveOp};
use crate::ty::{ResourceKind, Scalar, Space, Type, TypeData};

/// Where inference finds types.
pub trait Types {
    fn module(&self) -> &Module;
    /// The type `data` stands for.
    fn make(&mut self, data: TypeData) -> Result<Type, String>;
}

impl Types for Module {
    fn module(&self) -> &Module {
        self
    }

    fn make(&mut self, data: TypeData) -> Result<Type, String> {
        Ok(self.ty(data))
    }
}

/// A module that is not changed: a type it does not have yet cannot be any value's type.
pub struct Existing<'a>(pub &'a Module);

impl Types for Existing<'_> {
    fn module(&self) -> &Module {
        self.0
    }

    fn make(&mut self, data: TypeData) -> Result<Type, String> {
        self.0
            .types
            .lookup(&data)
            .ok_or_else(|| "the result's type is not in the module".to_string())
    }
}

fn scalar(t: &mut impl Types, s: Scalar) -> Result<Type, String> {
    t.make(TypeData::Scalar(s))
}

/// A vector, or the scalar itself for length 1.
fn vector(t: &mut impl Types, s: Scalar, n: u8) -> Result<Type, String> {
    if n == 1 {
        scalar(t, s)
    } else {
        t.make(TypeData::Vector(s, n))
    }
}

/// The result types of `op` applied to operands of types `args`. `explicit` is the type the
/// instruction states, for the operations that need one.
pub fn result_types(
    m: &mut Module,
    op: &Op,
    args: &[Type],
    explicit: Option<Type>,
) -> Result<Vec<Type>, String> {
    result_types_in(m, op, args, explicit)
}

/// [`result_types`] over any [`Types`].
pub fn result_types_in(
    t: &mut impl Types,
    op: &Op,
    args: &[Type],
    explicit: Option<Type>,
) -> Result<Vec<Type>, String> {
    let arg = |i: usize| -> Result<Type, String> {
        args.get(i)
            .copied()
            .ok_or_else(|| format!("expected at least {} operand(s)", i + 1))
    };
    let need_explicit = || explicit.ok_or_else(|| "this operation needs `: type`".to_string());
    let one = |ty: Result<Type, String>| ty.map(|ty| vec![ty]);
    match op {
        Op::Br(_)
        | Op::CondBr { .. }
        | Op::Switch { .. }
        | Op::Loop { .. }
        | Op::Return
        | Op::Unreachable
        | Op::Store { .. }
        | Op::Barrier { .. }
        | Op::Discard
        | Op::DebugPrint(_)
        | Op::DebugAssert(_) => Ok(vec![]),

        Op::Binary(_) | Op::Unary(_) | Op::Deriv { .. } => one(arg(0)),
        Op::Icmp(_) | Op::Fcmp(_) => {
            let ty = arg(0)?;
            let n = t
                .module()
                .width(ty)
                .ok_or("comparisons take scalars or vectors")?;
            one(vector(t, Scalar::I1, n))
        }
        Op::Select => one(arg(1)),
        Op::Convert(_) | Op::Splat | Op::Construct | Op::Var => one(need_explicit()),

        Op::Math(math) => {
            let ty = arg(0)?;
            match math {
                MathOp::Dot | MathOp::Length | MathOp::Distance => {
                    let s = t.module().element_scalar(ty).ok_or("expected a vector")?;
                    one(scalar(t, s))
                }
                MathOp::IsNan | MathOp::IsInf => {
                    let n = t.module().width(ty).ok_or("expected a scalar or vector")?;
                    one(vector(t, Scalar::I1, n))
                }
                _ => one(Ok(ty)),
            }
        }

        Op::Extract(_) | Op::ExtractDyn => match data(t, arg(0)?) {
            TypeData::Vector(s, _) => one(scalar(t, s)),
            TypeData::Matrix { element, rows, .. } => one(vector(t, element, rows)),
            _ => Err("expected a vector or matrix".into()),
        },
        Op::Insert(_) | Op::InsertDyn | Op::InsertField(_) | Op::InsertElem(_) => one(arg(0)),
        Op::Shuffle(lanes) => {
            let s = t
                .module()
                .element_scalar(arg(0)?)
                .ok_or("expected vectors")?;
            let n = u8::try_from(lanes.len()).map_err(|_| "too many lanes")?;
            one(vector(t, s, n))
        }

        Op::MatMul => match (data(t, arg(0)?), data(t, arg(1)?)) {
            (TypeData::Matrix { element, rows, .. }, TypeData::Matrix { cols, .. }) => {
                one(t.make(TypeData::Matrix {
                    element,
                    rows,
                    cols,
                }))
            }
            (TypeData::Matrix { element, rows, .. }, TypeData::Vector(..)) => {
                one(vector(t, element, rows))
            }
            (TypeData::Vector(..), TypeData::Matrix { element, cols, .. }) => {
                one(vector(t, element, cols))
            }
            _ => Err("matmul takes a matrix and a matrix or vector".into()),
        },
        Op::Transpose => match data(t, arg(0)?) {
            TypeData::Matrix {
                element,
                rows,
                cols,
            } => one(t.make(TypeData::Matrix {
                element,
                rows: cols,
                cols: rows,
            })),
            _ => Err("expected a matrix".into()),
        },
        Op::MatColumn(_) | Op::MatRow(_) | Op::MatExtract(..) => match data(t, arg(0)?) {
            TypeData::Matrix {
                element,
                rows,
                cols,
            } => match op {
                Op::MatColumn(_) => one(vector(t, element, rows)),
                Op::MatRow(_) => one(vector(t, element, cols)),
                _ => one(scalar(t, element)),
            },
            _ => Err("expected a matrix".into()),
        },

        Op::ExtractField(i) => match data(t, arg(0)?) {
            TypeData::Struct(s) => t.module().structs[s]
                .fields
                .get(*i as usize)
                .map(|f| vec![f.1])
                .ok_or_else(|| format!("no field {i}")),
            _ => Err("expected a struct".into()),
        },
        Op::ExtractElem(_) => match data(t, arg(0)?) {
            TypeData::Array(e, _) => one(Ok(e)),
            _ => Err("expected an array".into()),
        },

        Op::Load { .. } => match data(t, arg(0)?) {
            TypeData::Ptr { pointee, .. } => one(Ok(pointee)),
            _ => Err("expected a pointer".into()),
        },
        Op::Access(steps) => {
            let TypeData::Ptr {
                space,
                readonly,
                mut pointee,
            } = data(t, arg(0)?)
            else {
                return Err("expected a pointer".into());
            };
            for step in steps {
                pointee = step_type_in(t, pointee, *step)?;
            }
            one(t.make(TypeData::Ptr {
                space,
                readonly,
                pointee,
            }))
        }
        Op::BufferElem | Op::BufferByte | Op::UniformPtr => {
            let TypeData::Handle(kind) = data(t, arg(0)?) else {
                return Err("expected a handle".into());
            };
            let (wanted, readonly, space): (&[ResourceKind], bool, Space) = match op {
                Op::BufferElem => (
                    &[ResourceKind::Buffer, ResourceKind::ReadOnlyBuffer],
                    kind.kind == ResourceKind::ReadOnlyBuffer,
                    Space::Buffer,
                ),
                Op::BufferByte => (
                    &[ResourceKind::ByteBuffer, ResourceKind::ReadOnlyByteBuffer],
                    kind.kind == ResourceKind::ReadOnlyByteBuffer,
                    Space::Buffer,
                ),
                _ => (&[ResourceKind::Uniform], true, Space::Uniform),
            };
            if !wanted.contains(&kind.kind) {
                return Err(format!("not a handle of {}", wanted[0].name()));
            }
            let pointee = match kind.element {
                Some(e) => e,
                None => scalar(t, Scalar::I32)?,
            };
            one(t.make(TypeData::Ptr {
                space,
                readonly,
                pointee,
            }))
        }
        Op::BufferLength => one(scalar(t, Scalar::I32)),
        Op::DescGet { .. } => match data(t, arg(0)?) {
            TypeData::Res(kind) => one(t.make(TypeData::Handle(kind))),
            _ => Err("expected a resource index".into()),
        },

        Op::Atomic { op, .. } => {
            if *op == crate::op::AtomicOp::Store {
                return Ok(vec![]);
            }
            match data(t, arg(0)?) {
                TypeData::Ptr { pointee, .. } => one(Ok(pointee)),
                _ => Err("expected a pointer".into()),
            }
        }
        Op::IsHelper => one(scalar(t, Scalar::I1)),
        Op::Wave { op, .. } => match op {
            WaveOp::Reduce(_)
            | WaveOp::Scan { .. }
            | WaveOp::Broadcast
            | WaveOp::Shuffle
            | WaveOp::Quad(_) => one(arg(0)),
            WaveOp::Ballot | WaveOp::Match => one(vector(t, Scalar::I32, 4)),
            WaveOp::Vote(_) | WaveOp::IsFirst => one(scalar(t, Scalar::I1)),
            WaveOp::LaneIndex | WaveOp::LaneCount => one(scalar(t, Scalar::I32)),
        },
        Op::Call(f) => t
            .module()
            .funcs
            .get(*f)
            .map(|f| f.results.clone())
            .ok_or_else(|| "no such function".into()),
    }
}

fn data(t: &impl Types, ty: Type) -> TypeData {
    t.module().data(ty).clone()
}

/// The type one `access` step leads to from `ty`.
pub fn step_type(m: &mut Module, ty: Type, step: AccessStep) -> Result<Type, String> {
    step_type_in(m, ty, step)
}

/// [`step_type`] over any [`Types`].
pub fn step_type_in(t: &mut impl Types, ty: Type, step: AccessStep) -> Result<Type, String> {
    match (t.module().data(ty).clone(), step) {
        (TypeData::Struct(s), AccessStep::Field(i)) => t.module().structs[s]
            .fields
            .get(i as usize)
            .map(|f| f.1)
            .ok_or_else(|| format!("no field {i}")),
        (TypeData::Array(e, _), AccessStep::Index) => Ok(e),
        (TypeData::Vector(s, _), AccessStep::Index) => scalar(t, s),
        (TypeData::Matrix { element, rows, .. }, AccessStep::Index) => vector(t, element, rows),
        _ => Err("this step does not apply to the type".into()),
    }
}
