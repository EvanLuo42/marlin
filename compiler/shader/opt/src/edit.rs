//! Editing helpers the passes share: what an operation may be removed or folded for, and
//! terminators rebuilt with different jumps.

use marlin_shaderc_ir::{
    Block, ConstValue, Function, Inst, Module, Op, SwitchCase, Type, Val, Value, result_types,
    value_constant,
};

/// Operations whose only effect is their result: unused, they may go.
pub(crate) fn is_pure(op: &Op) -> bool {
    !(op.is_terminator()
        || matches!(
            op,
            Op::Store { .. }
                | Op::MeshSetCounts
                | Op::MeshDispatch
                | Op::TexStore(_)
                | Op::Atomic { .. }
                | Op::Barrier { .. }
                | Op::Discard
                | Op::Call(_)
                | Op::DebugPrint(_)
                | Op::DebugAssert(_)
        ))
}

/// Operations computed from their operands alone, which constant folding may evaluate.
pub(crate) fn is_foldable(op: &Op) -> bool {
    matches!(
        op,
        Op::Binary(_)
            | Op::Unary(_)
            | Op::Icmp(_)
            | Op::Fcmp(_)
            | Op::Select
            | Op::Convert(_)
            | Op::Math(_)
            | Op::Splat
            | Op::Extract(_)
            | Op::Insert(_)
            | Op::Shuffle(_)
            | Op::ExtractDyn
            | Op::InsertDyn
            | Op::MatMul
            | Op::Transpose
            | Op::MatColumn(_)
            | Op::MatRow(_)
            | Op::MatExtract(..)
            | Op::Construct
            | Op::ExtractField(_)
            | Op::InsertField(_)
            | Op::ExtractElem(_)
            | Op::InsertElem(_)
    )
}

/// A terminator's jumps: each target and its arguments.
pub(crate) fn jumps(f: &Function, term: Inst) -> Vec<(Block, Vec<Value>)> {
    let ops = f.operands(term);
    f.insts[term]
        .op
        .successors(ops.len())
        .into_iter()
        .map(|(b, r)| (b, ops[r].to_vec()))
        .collect()
}

/// The operands of a terminator that are not jump arguments: a condition, a scrutinee, the
/// returned values.
pub(crate) fn head_operands(f: &Function, term: Inst) -> Vec<Value> {
    let ops = f.operands(term);
    match &f.insts[term].op {
        Op::CondBr { .. } | Op::Switch { .. } => vec![ops[0]],
        Op::Return => ops.to_vec(),
        _ => Vec::new(),
    }
}

/// Rebuilds terminator `term` with new jumps (one per old jump, in order); the head operands
/// stay. Returns the new terminator.
pub(crate) fn set_jumps(f: &mut Function, term: Inst, new: Vec<(Block, Vec<Value>)>) -> Inst {
    let head = head_operands(f, term);
    let op = f.insts[term].op.clone();
    let mut operands = head;
    let op = match op {
        Op::Br(_) => {
            let (b, args) = new.into_iter().next().expect("a jump");
            operands.extend(args);
            Op::Br(b)
        }
        Op::Loop {
            continuing, merge, ..
        } => {
            let (b, args) = new.into_iter().next().expect("a jump");
            operands.extend(args);
            Op::Loop {
                header: b,
                continuing,
                merge,
            }
        }
        Op::CondBr { merge, .. } => {
            let mut it = new.into_iter();
            let (t, targs) = it.next().expect("a then jump");
            let (e, eargs) = it.next().expect("an else jump");
            let then_args = targs.len() as u32;
            operands.extend(targs);
            operands.extend(eargs);
            Op::CondBr {
                then_block: t,
                then_args,
                else_block: e,
                merge,
            }
        }
        Op::Switch { cases, merge, .. } => {
            let mut it = new.into_iter();
            let mut out = Vec::new();
            for case in cases {
                let (b, args) = it.next().expect("a case jump");
                out.push(SwitchCase {
                    value: case.value,
                    block: b,
                    args: args.len() as u32,
                });
                operands.extend(args);
            }
            let (d, dargs) = it.next().expect("a default jump");
            let default_args = dargs.len() as u32;
            operands.extend(dargs);
            Op::Switch {
                cases: out,
                default: d,
                default_args,
                merge,
            }
        }
        other => other,
    };
    f.replace_inst(term, op, operands)
}

/// Replaces terminator `term` by a jump to `target`.
pub(crate) fn set_br(f: &mut Function, term: Inst, target: Block, args: Vec<Value>) -> Inst {
    f.replace_inst(term, Op::Br(target), args)
}

/// A new instruction right before `before`, with inferred result types (or `explicit`); its
/// single result.
pub(crate) fn value_before(
    m: &mut Module,
    f: &mut Function,
    before: Inst,
    op: Op,
    operands: Vec<Value>,
    explicit: Option<Type>,
) -> Value {
    let types: Vec<Type> = operands.iter().map(|&v| f.ty(v)).collect();
    let results = result_types(m, &op, &types, explicit)
        .unwrap_or_else(|e| panic!("a pass built an operation that does not type: {e}"));
    let inst = f.create_inst(op, operands, &results);
    f.insert_before(before, inst);
    f.result(inst)
}

/// The constant `v` of type `t` as a value of `f`.
pub(crate) fn constant(m: &mut Module, f: &mut Function, v: &Val, t: Type) -> Value {
    let c = value_constant(m, v, t).expect("a constant of a data type");
    f.const_value(c, t)
}

/// The all-zero constant of type `t` as a value of `f`.
pub(crate) fn zero(m: &mut Module, f: &mut Function, t: Type) -> Value {
    let c = m.constant(t, ConstValue::Zero);
    f.const_value(c, t)
}
