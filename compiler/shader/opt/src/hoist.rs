//! Constant local arrays become module data (0005 §4.4 pass 1.7): a `var` of array type that
//! is stored once, with a constant, before anything reads it, and is then only read — the shape
//! lowering gives `a[i]` on a constant array — becomes a `constant` module variable with that
//! value. Equal arrays share one variable.

use marlin_shaderc_ir::cfg::Cfg;
use marlin_shaderc_ir::{
    Binding, FuncId, Function, GlobalData, Inst, Module, Op, Space, TypeData, Value, ValueDef,
};

use crate::edit::value_before;
use crate::{Analyses, Changed, Pass};

pub struct HoistConstArrays;

impl Pass for HoistConstArrays {
    fn name(&self) -> &'static str {
        "hoist-const-arrays"
    }

    fn run(&self, m: &mut Module, _: &mut Analyses) -> bool {
        let ids: Vec<FuncId> = m.funcs.iter().map(|(id, _)| id).collect();
        let mut any = false;
        for id in ids {
            let mut f = std::mem::replace(&mut m.funcs[id], Function::new("", Vec::new()));
            any |= hoist_in(m, &mut f) != Changed::No;
            m.funcs[id] = f;
        }
        // The new constant data is used by the entry points that reach it.
        if any {
            m.recompute_uses();
        }
        any
    }
}

fn hoist_in(m: &mut Module, f: &mut Function) -> Changed {
    let vars: Vec<Inst> = f
        .layout
        .iter()
        .flat_map(|&b| f.insts(b).collect::<Vec<_>>())
        .filter(|&i| f.insts[i].op == Op::Var)
        .collect();
    if vars.is_empty() {
        return Changed::No;
    }
    let cfg = Cfg::new(f);
    let mut changed = Changed::No;
    for var in vars {
        if let Some(store) = hoistable(m, f, &cfg, var) {
            hoist(m, f, var, store);
            changed = Changed::Values;
        }
    }
    changed
}

/// Whether only loads read through `p` (directly or through `access`).
fn read_only(f: &Function, p: Value) -> bool {
    f.uses(p).iter().all(|u| match f.insts[u.inst].op {
        Op::Load { .. } => true,
        Op::Access(_) => u.index == 0 && read_only(f, f.result(u.inst)),
        _ => false,
    })
}

/// The one store of a constant into `var` that comes before every read, if that is all.
fn hoistable(m: &Module, f: &Function, cfg: &Cfg, var: Inst) -> Option<Inst> {
    let ptr = f.result(var);
    let TypeData::Ptr { pointee, .. } = m.data(f.ty(ptr)) else {
        return None;
    };
    if !matches!(m.data(*pointee), TypeData::Array(..)) {
        return None;
    }
    let mut store = None;
    for u in f.uses(ptr) {
        match f.insts[u.inst].op {
            Op::Store { .. } => {
                let v = f.operands(u.inst)[1];
                if store.is_some() || u.index != 0 || !matches!(f.values[v].def, ValueDef::Const(_))
                {
                    return None;
                }
                store = Some(u.inst);
            }
            Op::Load { .. } => {}
            Op::Access(_) if u.index == 0 => {
                if !read_only(f, f.result(u.inst)) {
                    return None;
                }
            }
            _ => return None,
        }
    }
    let store = store?;
    // The store comes before every other use: later in its block, or in blocks it dominates.
    let block = f.block_of(store)?;
    let position = |i: Inst| f.insts(block).position(|x| x == i);
    let at = position(store)?;
    let home = cfg.index[&block];
    let before_all = f.uses(ptr).iter().all(|u| {
        u.inst == store
            || match f.block_of(u.inst) {
                Some(b) if b == block => position(u.inst).is_some_and(|k| k > at),
                Some(b) => cfg.dominates(home, cfg.index[&b]),
                None => false,
            }
    });
    before_all.then_some(store)
}

fn hoist(m: &mut Module, f: &mut Function, var: Inst, store: Inst) {
    let ptr = f.result(var);
    let TypeData::Ptr { pointee, .. } = *m.data(f.ty(ptr)) else {
        unreachable!()
    };
    let ValueDef::Const(c) = f.values[f.operands(store)[1]].def else {
        unreachable!()
    };
    let gptr = m.ptr(Space::Constant, false, pointee);
    let existing = m
        .globals
        .iter()
        .find(|(_, g)| g.ty == gptr && g.init == Some(c))
        .map(|(id, _)| id);
    let g = existing.unwrap_or_else(|| {
        let name = format!("{}.const{}", f.name, m.globals.len());
        m.add_global(GlobalData {
            name: name.into(),
            ty: gptr,
            binding: Binding::None,
            interp: None,
            init: Some(c),
        })
    });
    let base = f.global_value(g, gptr);
    f.remove_inst(store);
    rebase(m, f, ptr, base);
    f.remove_inst(var);
}

/// Makes everything that reads through `old` read through `new` (a pointer into another
/// space): `access` instructions are made again, since their result types change.
fn rebase(m: &mut Module, f: &mut Function, old: Value, new: Value) {
    for u in f.uses(old).to_vec() {
        let inst = u.inst;
        match f.insts[inst].op.clone() {
            Op::Access(steps) => {
                let mut operands = f.operands(inst).to_vec();
                operands[0] = new;
                let made = value_before(m, f, inst, Op::Access(steps), operands, None);
                let r = f.result(inst);
                rebase(m, f, r, made);
                f.remove_inst(inst);
            }
            _ => f.set_operand(inst, u.index as usize, new),
        }
    }
}
