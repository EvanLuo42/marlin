//! Promoting variables to SSA values (0005 §4.4 pass 1.3). A `var` that is only loaded from and
//! stored to as a whole becomes values: block parameters where stores from different paths
//! meet (the iterated dominance frontier of the stores, inside what the `var` dominates), and
//! the stored value everywhere else. A `var` starts zeroed, as the interpreter has it.

use std::collections::{HashMap, HashSet};

use marlin_shaderc_ir::cfg::Cfg;
use marlin_shaderc_ir::{Block, FuncId, Function, Inst, Module, Op, TypeData, Value, ValueDef};

use crate::edit::{jumps, set_jumps, zero};
use crate::{Analyses, Changed, FunctionPass};

pub struct Mem2Reg;

impl FunctionPass for Mem2Reg {
    fn name(&self) -> &'static str {
        "mem2reg"
    }

    fn run_function(
        &self,
        m: &mut Module,
        _: FuncId,
        f: &mut Function,
        _: &mut Analyses,
    ) -> Changed {
        let vars: Vec<Inst> = f
            .layout
            .iter()
            .flat_map(|&b| f.insts(b).collect::<Vec<_>>())
            .filter(|&i| promotable(m, f, i))
            .collect();
        if vars.is_empty() {
            return Changed::No;
        }
        let cfg = Cfg::new(f);
        let frontier = frontiers(&cfg);
        for var in vars {
            promote(m, f, &cfg, &frontier, var);
        }
        Changed::Values
    }
}

/// A `var` of plain data whose address is only loaded from and stored to.
fn promotable(m: &Module, f: &Function, inst: Inst) -> bool {
    if f.insts[inst].op != Op::Var {
        return false;
    }
    let ptr = f.result(inst);
    let TypeData::Ptr { pointee, .. } = m.data(f.ty(ptr)) else {
        return false;
    };
    if matches!(
        m.data(*pointee),
        TypeData::RayQuery | TypeData::Ptr { .. } | TypeData::Handle(_)
    ) {
        return false;
    }
    f.uses(ptr).iter().all(|u| match f.insts[u.inst].op {
        Op::Load { .. } => true,
        Op::Store { .. } => u.index == 0 && f.operands(u.inst)[1] != ptr,
        _ => false,
    })
}

/// Dominance frontiers of the reached blocks.
fn frontiers(cfg: &Cfg) -> Vec<Vec<usize>> {
    let mut df = vec![Vec::new(); cfg.len()];
    for b in 0..cfg.len() {
        if !cfg.reachable(b) || cfg.preds[b].len() < 2 {
            continue;
        }
        let idom = cfg.idom[b].expect("reached");
        for &p in &cfg.preds[b] {
            let mut runner = p;
            while runner != idom {
                if !df[runner].contains(&b) {
                    df[runner].push(b);
                }
                match cfg.idom[runner] {
                    Some(up) if up != runner => runner = up,
                    _ => break,
                }
            }
        }
    }
    df
}

fn promote(m: &mut Module, f: &mut Function, cfg: &Cfg, frontier: &[Vec<usize>], var: Inst) {
    let ptr = f.result(var);
    let TypeData::Ptr { pointee, .. } = *m.data(f.ty(ptr)) else {
        return;
    };
    let home = cfg.index[&f.block_of(var).expect("a placed var")];
    if !cfg.reachable(home) {
        return;
    }
    // Blocks that get a parameter for the variable.
    let mut defs: Vec<usize> = vec![home];
    for u in f.uses(ptr).to_vec() {
        if matches!(f.insts[u.inst].op, Op::Store { .. }) {
            defs.push(cfg.index[&f.block_of(u.inst).expect("a placed store")]);
        }
    }
    let mut phis: HashSet<usize> = HashSet::new();
    let mut work = defs.clone();
    while let Some(d) = work.pop() {
        for &y in &frontier[d] {
            if y != home && cfg.dominates(home, y) && phis.insert(y) {
                work.push(y);
            }
        }
    }
    let mut param: HashMap<usize, Value> = HashMap::new();
    for &y in &phis {
        param.insert(y, f.add_param(cfg.blocks[y], pointee, None));
    }

    // Children in the dominator tree.
    let mut children = vec![Vec::new(); cfg.len()];
    for (b, idom) in cfg.idom.iter().enumerate() {
        if let Some(d) = *idom
            && d != b
        {
            children[d].push(b);
        }
    }
    let initial = zero(m, f, pointee);
    let mut end: HashMap<usize, Value> = HashMap::new();
    let mut dead: Vec<Inst> = Vec::new();
    // Walk the blocks `home` dominates, each with the value on entry.
    let mut stack: Vec<(usize, Option<Value>)> = vec![(home, None)];
    while let Some((bi, incoming)) = stack.pop() {
        let block = cfg.blocks[bi];
        let mut current = param.get(&bi).copied().or(incoming);
        for inst in f.insts(block).collect::<Vec<_>>() {
            if inst == var {
                current = Some(initial);
                continue;
            }
            let ops = f.operands(inst);
            match f.insts[inst].op {
                Op::Load { .. } if ops[0] == ptr => {
                    let v = current.unwrap_or(initial);
                    let r = f.result(inst);
                    f.replace_all_uses(r, v);
                    dead.push(inst);
                }
                Op::Store { .. } if ops[0] == ptr => {
                    current = Some(ops[1]);
                    dead.push(inst);
                }
                _ => {}
            }
        }
        end.insert(bi, current.unwrap_or(initial));
        for &c in &children[bi] {
            stack.push((c, Some(end[&bi])));
        }
    }

    // Every jump to a block with a parameter passes the value at its end.
    if !phis.is_empty() {
        for (pi, &pb) in cfg.blocks.iter().enumerate() {
            let Some(t) = f.terminator(pb) else { continue };
            let js = jumps(f, t);
            if !js
                .iter()
                .any(|(target, _)| param.contains_key(&cfg.index[target]))
            {
                continue;
            }
            let value = end.get(&pi).copied().unwrap_or(initial);
            let new = js
                .into_iter()
                .map(|(target, mut args)| {
                    if param.contains_key(&cfg.index[&target]) {
                        args.push(value);
                    }
                    (target, args)
                })
                .collect::<Vec<(Block, Vec<Value>)>>();
            set_jumps(f, t, new);
        }
    }
    // Loads and stores the walk did not reach are in unreachable code.
    for u in f.uses(ptr).to_vec() {
        if dead.contains(&u.inst) {
            continue;
        }
        if matches!(f.insts[u.inst].op, Op::Load { .. }) {
            let r = f.result(u.inst);
            f.replace_all_uses(r, initial);
        }
        dead.push(u.inst);
    }
    f.remove_insts(&dead);
    f.remove_inst(var);
    debug_assert!(matches!(f.values[ptr].def, ValueDef::Result(..)));
}
