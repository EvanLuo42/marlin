//! Dead code elimination (0005 §4.4 pass 2.10, without control dependence): marks what side
//! effects, branch conditions and returned values need, through instructions and block
//! parameters, and deletes the rest. A parameter is live only if something live reads it, so
//! a loop variable that only feeds itself goes too.

use std::collections::{HashMap, HashSet};

use marlin_shaderc_ir::{Block, FuncId, Function, Inst, Module, Value, ValueDef};

use crate::edit::{head_operands, is_pure, jumps, set_jumps};
use crate::{Analyses, Changed, FunctionPass};

pub struct Dce;

impl FunctionPass for Dce {
    fn name(&self) -> &'static str {
        "dce"
    }

    fn run_function(
        &self,
        _: &mut Module,
        _: FuncId,
        f: &mut Function,
        _: &mut Analyses,
    ) -> Changed {
        if run(f) { Changed::Values } else { Changed::No }
    }
}

fn run(f: &mut Function) -> bool {
    let Some(entry) = f.entry() else {
        return false;
    };
    // Who jumps to each block, and with which arguments.
    let mut into: HashMap<Block, Vec<Vec<Value>>> = HashMap::new();
    for &b in &f.layout {
        if let Some(t) = f.terminator(b) {
            for (target, args) in jumps(f, t) {
                into.entry(target).or_default().push(args);
            }
        }
    }

    let mut live_insts: HashSet<Inst> = HashSet::new();
    let mut live_params: HashSet<Value> = f.params().iter().copied().collect();
    let mut work: Vec<Value> = Vec::new();
    for &b in &f.layout {
        for inst in f.insts(b) {
            let op = &f.insts[inst].op;
            if op.is_terminator() {
                live_insts.insert(inst);
                work.extend(head_operands(f, inst));
            } else if !is_pure(op) {
                live_insts.insert(inst);
                work.extend_from_slice(f.operands(inst));
            }
        }
    }
    while let Some(v) = work.pop() {
        match f.values[v].def {
            ValueDef::Result(inst, _) => {
                if live_insts.insert(inst) {
                    work.extend_from_slice(f.operands(inst));
                }
            }
            ValueDef::Param(b, i) => {
                if live_params.insert(v) {
                    for args in into.get(&b).into_iter().flatten() {
                        if let Some(&a) = args.get(i as usize) {
                            work.push(a);
                        }
                    }
                }
            }
            ValueDef::Const(_) | ValueDef::Global(_) => {}
        }
    }

    let mut changed = false;
    // Dead parameters, and the arguments jumps pass them.
    let dead_params: HashMap<Block, Vec<usize>> = f
        .layout
        .iter()
        .filter(|&&b| b != entry)
        .filter_map(|&b| {
            let dead: Vec<usize> = f.blocks[b]
                .params
                .iter()
                .enumerate()
                .filter(|(_, p)| !live_params.contains(p))
                .map(|(i, _)| i)
                .collect();
            (!dead.is_empty()).then_some((b, dead))
        })
        .collect();
    if !dead_params.is_empty() {
        changed = true;
        let layout = f.layout.clone();
        for &b in &layout {
            let Some(t) = f.terminator(b) else { continue };
            let js = jumps(f, t);
            if !js
                .iter()
                .any(|(target, _)| dead_params.contains_key(target))
            {
                continue;
            }
            let new = js
                .into_iter()
                .map(|(target, args)| {
                    let dead = dead_params.get(&target);
                    let args = args
                        .into_iter()
                        .enumerate()
                        .filter(|(i, _)| dead.is_none_or(|d| !d.contains(i)))
                        .map(|(_, a)| a)
                        .collect();
                    (target, args)
                })
                .collect();
            let new_term = set_jumps(f, t, new);
            live_insts.insert(new_term);
        }
    }
    // Dead instructions (their results may feed only each other now).
    let dead: Vec<Inst> = f
        .layout
        .iter()
        .flat_map(|&b| f.insts(b).collect::<Vec<_>>())
        .filter(|i| !live_insts.contains(i))
        .collect();
    if !dead.is_empty() {
        changed = true;
        f.remove_insts(&dead);
    }
    for (b, mut dead) in dead_params {
        dead.sort_unstable();
        for i in dead.into_iter().rev() {
            f.remove_param(b, i);
        }
    }
    changed
}
