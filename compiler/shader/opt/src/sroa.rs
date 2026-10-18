//! Scalar replacement of aggregates (0005 §4.4 pass 1.2): a `var` of struct or array type (an
//! array of at most [`MAX_ELEMENTS`]) whose every use names one part with a constant — an
//! `access` whose first step is a field or a constant index, or a whole `load` or `store` —
//! becomes one `var` per part. Parts are split again until nothing splits, for mem2reg to
//! promote.

use marlin_shaderc_ir::{
    AccessStep, FuncId, Function, Inst, Module, Op, Space, Type, TypeData, Value, ValueDef,
    constant_value,
};

use crate::edit::value_before;
use crate::{Analyses, Changed, FunctionPass};

/// Larger arrays stay whole.
pub const MAX_ELEMENTS: u32 = 64;

pub struct Sroa;

impl FunctionPass for Sroa {
    fn name(&self) -> &'static str {
        "sroa"
    }

    fn run_function(
        &self,
        m: &mut Module,
        _: FuncId,
        f: &mut Function,
        _: &mut Analyses,
    ) -> Changed {
        let mut changed = Changed::No;
        loop {
            let vars: Vec<Inst> = f
                .layout
                .iter()
                .flat_map(|&b| f.insts(b).collect::<Vec<_>>())
                .filter(|&i| f.insts[i].op == Op::Var)
                .collect();
            let mut any = false;
            for var in vars {
                if let Some(parts) = parts(m, f, var) {
                    split(m, f, var, &parts);
                    any = true;
                }
            }
            if !any {
                return changed;
            }
            changed = Changed::Values;
        }
    }
}

/// The part types of a splittable `var`.
fn parts(m: &Module, f: &Function, var: Inst) -> Option<Vec<Type>> {
    let ptr = f.result(var);
    let TypeData::Ptr { pointee, .. } = m.data(f.ty(ptr)) else {
        return None;
    };
    let types = match m.data(*pointee) {
        TypeData::Struct(id) => m.structs[*id]
            .fields
            .iter()
            .map(|x| x.1)
            .collect::<Vec<_>>(),
        TypeData::Array(e, n) if *n <= MAX_ELEMENTS => vec![*e; *n as usize],
        _ => return None,
    };
    let ok = f.uses(ptr).iter().all(|u| {
        let ops = f.operands(u.inst);
        match &f.insts[u.inst].op {
            Op::Load { .. } => true,
            Op::Store { .. } => u.index == 0 && ops[1] != ptr,
            Op::Access(steps) => {
                u.index == 0 && part_index(m, f, steps, ops).is_some_and(|i| i < types.len())
            }
            _ => false,
        }
    });
    ok.then_some(types)
}

/// The part an `access` starts with, if its first step is constant.
fn part_index(m: &Module, f: &Function, steps: &[AccessStep], ops: &[Value]) -> Option<usize> {
    match steps.first()? {
        AccessStep::Field(i) => Some(*i as usize),
        AccessStep::Index => match f.values[ops[1]].def {
            ValueDef::Const(c) => constant_value(m, c).ok()?.bits().map(|b| b as usize),
            _ => None,
        },
    }
}

fn split(m: &mut Module, f: &mut Function, var: Inst, types: &[Type]) {
    let ptr = f.result(var);
    let TypeData::Ptr { pointee, .. } = *m.data(f.ty(ptr)) else {
        unreachable!()
    };
    let is_struct = matches!(m.data(pointee), TypeData::Struct(_));
    let mut parts = Vec::new();
    for &t in types {
        let pt = m.ptr(Space::Function, false, t);
        parts.push(value_before(m, f, var, Op::Var, Vec::new(), Some(pt)));
    }
    for u in f.uses(ptr).to_vec() {
        let inst = u.inst;
        let ops = f.operands(inst).to_vec();
        match f.insts[inst].op.clone() {
            Op::Load { aligned } => {
                let mut loaded = Vec::new();
                for (&p, &t) in parts.iter().zip(types) {
                    loaded.push(value_before(
                        m,
                        f,
                        inst,
                        Op::Load { aligned },
                        vec![p],
                        Some(t),
                    ));
                }
                let whole = value_before(m, f, inst, Op::Construct, loaded, Some(pointee));
                let r = f.result(inst);
                f.replace_all_uses(r, whole);
                f.remove_inst(inst);
            }
            Op::Store { aligned } => {
                for (i, &p) in parts.iter().enumerate() {
                    let op = if is_struct {
                        Op::ExtractField(i as u32)
                    } else {
                        Op::ExtractElem(i as u32)
                    };
                    let x = value_before(m, f, inst, op, vec![ops[1]], None);
                    let s = f.create_inst(Op::Store { aligned }, vec![p, x], &[]);
                    f.insert_before(inst, s);
                }
                f.remove_inst(inst);
            }
            Op::Access(steps) => {
                let k = part_index(m, f, &steps, &ops).expect("a constant part");
                let skip = if steps[0] == AccessStep::Index { 2 } else { 1 };
                let new = if steps.len() == 1 {
                    parts[k]
                } else {
                    let mut operands = vec![parts[k]];
                    operands.extend_from_slice(&ops[skip..]);
                    value_before(m, f, inst, Op::Access(steps[1..].to_vec()), operands, None)
                };
                let r = f.result(inst);
                f.replace_all_uses(r, new);
                f.remove_inst(inst);
            }
            _ => unreachable!("checked by `parts`"),
        }
    }
    f.remove_inst(var);
}
