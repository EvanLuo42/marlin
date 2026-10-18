//! `[Precise]` propagation (0005 §4.4 pass 1.6): a precise result is only as precise as what it
//! is computed from, so the operations that produce a precise operation's operands (through
//! block parameters and through any operation in between) become precise too. Later passes
//! leave precise operations alone: no reassociation, contraction or approximation.

use std::collections::{HashMap, HashSet};

use marlin_shaderc_ir::{Block, FuncId, Function, Inst, Module, Op, UnaryOp, Value, ValueDef};

use crate::edit::jumps;
use crate::{Analyses, Changed, FunctionPass};

pub struct PrecisePropagate;

/// The operations the flag means something for (and the text format shows it on).
fn takes_flag(op: &Op) -> bool {
    match op {
        Op::Binary(b) => b.is_float(),
        Op::Unary(u) => *u == UnaryOp::FNeg,
        Op::Fcmp(_) | Op::Convert(_) | Op::Math(_) | Op::MatMul => true,
        _ => false,
    }
}

impl FunctionPass for PrecisePropagate {
    fn name(&self) -> &'static str {
        "precise"
    }

    fn run_function(
        &self,
        _: &mut Module,
        _: FuncId,
        f: &mut Function,
        _: &mut Analyses,
    ) -> Changed {
        let insts: Vec<Inst> = f
            .layout
            .iter()
            .flat_map(|&b| f.insts(b).collect::<Vec<_>>())
            .collect();
        let mut work: Vec<Value> = Vec::new();
        for &i in &insts {
            if f.insts[i].precise {
                work.extend_from_slice(f.operands(i));
            }
        }
        if work.is_empty() {
            return Changed::No;
        }
        let mut into: HashMap<Block, Vec<Vec<Value>>> = HashMap::new();
        for &b in &f.layout {
            if let Some(t) = f.terminator(b) {
                for (target, args) in jumps(f, t) {
                    into.entry(target).or_default().push(args);
                }
            }
        }
        let mut seen: HashSet<Value> = HashSet::new();
        let mut changed = Changed::No;
        while let Some(v) = work.pop() {
            if !seen.insert(v) {
                continue;
            }
            match f.values[v].def {
                ValueDef::Result(i, _) => {
                    if !f.insts[i].precise && takes_flag(&f.insts[i].op) {
                        f.insts[i].precise = true;
                        changed = Changed::Values;
                    }
                    work.extend_from_slice(f.operands(i));
                }
                ValueDef::Param(b, k) => {
                    for args in into.get(&b).into_iter().flatten() {
                        if let Some(&a) = args.get(k as usize) {
                            work.push(a);
                        }
                    }
                }
                _ => {}
            }
        }
        changed
    }
}
