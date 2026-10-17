//! The result types of an operation from its operands' types. The builder and the text parser
//! use it, so most instructions never spell out their types; the few whose result cannot be
//! inferred (conversions, `splat`, `construct`, `var`) take it explicitly. It reports operands
//! it cannot make sense of; whether the operands are otherwise valid is the verifier's job.

use crate::module::Module;
use crate::op::{AccessStep, MathOp, Op, WaveOp};
use crate::ty::{ResourceKind, Scalar, Space, Type, TypeData};

/// The result types of `op` applied to operands of types `args`. `explicit` is the type the
/// instruction states, for the operations that need one.
pub fn result_types(
    m: &mut Module,
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
    let one = |t: Type| Ok(vec![t]);
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

        Op::Binary(_) | Op::Unary(_) | Op::Deriv { .. } => one(arg(0)?),
        Op::Icmp(_) | Op::Fcmp(_) => {
            let t = arg(0)?;
            let n = m.width(t).ok_or("comparisons take scalars or vectors")?;
            one(m.vector(Scalar::I1, n))
        }
        Op::Select => one(arg(1)?),
        Op::Convert(_) | Op::Splat | Op::Construct | Op::Var => one(need_explicit()?),

        Op::Math(math) => {
            let t = arg(0)?;
            match math {
                MathOp::Dot | MathOp::Length | MathOp::Distance => {
                    let s = m.element_scalar(t).ok_or("expected a vector")?;
                    one(m.scalar(s))
                }
                MathOp::IsNan | MathOp::IsInf => {
                    let n = m.width(t).ok_or("expected a scalar or vector")?;
                    one(m.vector(Scalar::I1, n))
                }
                _ => one(t),
            }
        }

        Op::Extract(_) | Op::ExtractDyn => {
            let t = arg(0)?;
            match m.data(t).clone() {
                TypeData::Vector(s, _) => one(m.scalar(s)),
                TypeData::Matrix { element, rows, .. } => one(m.vector(element, rows)),
                _ => Err("expected a vector or matrix".into()),
            }
        }
        Op::Insert(_) | Op::InsertDyn | Op::InsertField(_) | Op::InsertElem(_) => one(arg(0)?),
        Op::Shuffle(lanes) => {
            let s = m.element_scalar(arg(0)?).ok_or("expected vectors")?;
            let n = u8::try_from(lanes.len()).map_err(|_| "too many lanes")?;
            one(m.vector(s, n))
        }

        Op::MatMul => {
            let (a, b) = (m.data(arg(0)?).clone(), m.data(arg(1)?).clone());
            match (a, b) {
                (TypeData::Matrix { element, rows, .. }, TypeData::Matrix { cols, .. }) => one(m
                    .ty(TypeData::Matrix {
                        element,
                        rows,
                        cols,
                    })),
                (TypeData::Matrix { element, rows, .. }, TypeData::Vector(..)) => {
                    one(m.vector(element, rows))
                }
                (TypeData::Vector(..), TypeData::Matrix { element, cols, .. }) => {
                    one(m.vector(element, cols))
                }
                _ => Err("matmul takes a matrix and a matrix or vector".into()),
            }
        }
        Op::Transpose => match m.data(arg(0)?).clone() {
            TypeData::Matrix {
                element,
                rows,
                cols,
            } => one(m.ty(TypeData::Matrix {
                element,
                rows: cols,
                cols: rows,
            })),
            _ => Err("expected a matrix".into()),
        },
        Op::MatColumn(_) | Op::MatRow(_) | Op::MatExtract(..) => match m.data(arg(0)?).clone() {
            TypeData::Matrix {
                element,
                rows,
                cols,
            } => match op {
                Op::MatColumn(_) => one(m.vector(element, rows)),
                Op::MatRow(_) => one(m.vector(element, cols)),
                _ => one(m.scalar(element)),
            },
            _ => Err("expected a matrix".into()),
        },

        Op::ExtractField(i) => match m.data(arg(0)?).clone() {
            TypeData::Struct(s) => m.structs[s]
                .fields
                .get(*i as usize)
                .map(|f| vec![f.1])
                .ok_or_else(|| format!("no field {i}")),
            _ => Err("expected a struct".into()),
        },
        Op::ExtractElem(_) => match m.data(arg(0)?).clone() {
            TypeData::Array(e, _) => one(e),
            _ => Err("expected an array".into()),
        },

        Op::Load { .. } => match m.data(arg(0)?).clone() {
            TypeData::Ptr { pointee, .. } => one(pointee),
            _ => Err("expected a pointer".into()),
        },
        Op::Access(steps) => {
            let TypeData::Ptr {
                space,
                readonly,
                mut pointee,
            } = m.data(arg(0)?).clone()
            else {
                return Err("expected a pointer".into());
            };
            for step in steps {
                pointee = step_type(m, pointee, *step)?;
            }
            one(m.ptr(space, readonly, pointee))
        }
        Op::BufferElem | Op::BufferByte | Op::UniformPtr => {
            let TypeData::Handle(kind) = m.data(arg(0)?).clone() else {
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
                None => m.scalar(Scalar::I32),
            };
            one(m.ptr(space, readonly, pointee))
        }
        Op::BufferLength => one(m.scalar(Scalar::I32)),
        Op::DescGet { .. } => match m.data(arg(0)?).clone() {
            TypeData::Res(kind) => one(m.ty(TypeData::Handle(kind))),
            _ => Err("expected a resource index".into()),
        },

        Op::Atomic { op, .. } => {
            if *op == crate::op::AtomicOp::Store {
                return Ok(vec![]);
            }
            match m.data(arg(0)?).clone() {
                TypeData::Ptr { pointee, .. } => one(pointee),
                _ => Err("expected a pointer".into()),
            }
        }
        Op::IsHelper => one(m.scalar(Scalar::I1)),
        Op::Wave { op, .. } => match op {
            WaveOp::Reduce(_)
            | WaveOp::Scan { .. }
            | WaveOp::Broadcast
            | WaveOp::Shuffle
            | WaveOp::Quad(_) => one(arg(0)?),
            WaveOp::Ballot | WaveOp::Match => one(m.vector(Scalar::I32, 4)),
            WaveOp::Vote(_) | WaveOp::IsFirst => one(m.scalar(Scalar::I1)),
            WaveOp::LaneIndex | WaveOp::LaneCount => one(m.scalar(Scalar::I32)),
        },
        Op::Call(f) => m
            .funcs
            .get(*f)
            .map(|f| f.results.clone())
            .ok_or_else(|| "no such function".into()),
    }
}

/// The type one `access` step leads to from `ty`.
pub fn step_type(m: &mut Module, ty: Type, step: AccessStep) -> Result<Type, String> {
    match (m.data(ty).clone(), step) {
        (TypeData::Struct(s), AccessStep::Field(i)) => m.structs[s]
            .fields
            .get(i as usize)
            .map(|f| f.1)
            .ok_or_else(|| format!("no field {i}")),
        (TypeData::Array(e, _), AccessStep::Index) => Ok(e),
        (TypeData::Vector(s, _), AccessStep::Index) => Ok(m.scalar(s)),
        (TypeData::Matrix { element, rows, .. }, AccessStep::Index) => Ok(m.vector(element, rows)),
        _ => Err("this step does not apply to the type".into()),
    }
}
