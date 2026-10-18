//! Structure-preserving CFG simplification (0005 §4.4 pass 2.12). Each rule keeps every region
//! single-entry, single-exit and nested (0006 §5.1):
//!
//! - A `cond_br` on a constant, or whose two jumps are the same, becomes a `br`. Edges into a
//!   selection's merge block come only from the selection's own blocks (nothing breaks out to
//!   a selection's merge), so its blocks join the region around it unchanged.
//! - A `switch` on a constant keeps only the jump it takes, and a `switch` whose cases all go
//!   where the default does keeps none: a `switch` without cases is still a region, because
//!   cases may break out to its merge from inside nested regions. Once nothing does, it
//!   becomes a `br`.
//! - A loop whose continue block is unreachable and whose header nothing jumps back to runs
//!   once: its `loop` becomes a `br` once nothing breaks out of a nested region to its merge.
//! - Unreachable blocks go; an unreachable merge or continue block of a reachable region stays,
//!   empty, as `unreachable`.
//! - A block that only one block jumps to, with a `br`, joins that block; a block that only
//!   jumps on is jumped over. Merge, continue and loop header blocks stay.

use std::collections::HashSet;

use marlin_shaderc_ir::cfg::{Cfg, RegionKind, Structure};
use marlin_shaderc_ir::{Block, FuncId, Function, Module, Op, Value, ValueDef};

use crate::edit::{jumps, set_br, set_jumps};
use crate::{Analyses, Changed, FunctionPass};

pub struct SimplifyCfg;

impl FunctionPass for SimplifyCfg {
    fn name(&self) -> &'static str {
        "simplify-cfg"
    }

    fn run_function(
        &self,
        m: &mut Module,
        _: FuncId,
        f: &mut Function,
        _: &mut Analyses,
    ) -> Changed {
        let mut changed = false;
        // Each round makes one kind of change and the analyses are rebuilt.
        while round(m, f) {
            changed = true;
        }
        if changed { Changed::Cfg } else { Changed::No }
    }
}

fn round(m: &Module, f: &mut Function) -> bool {
    if f.layout.is_empty() {
        return false;
    }
    if fold_terminators(m, f) {
        return true;
    }
    let cfg = Cfg::new(f);
    let st = Structure::new(f, &cfg);
    remove_unreachable(f, &cfg, &st)
        || degenerate_regions(f, &cfg, &st)
        || merge_chains(f, &cfg, &st)
        || jump_over_empty(f, &cfg, &st)
}

fn const_bits(m: &Module, f: &Function, v: Value) -> Option<u64> {
    match f.values[v].def {
        ValueDef::Const(c) => marlin_shaderc_ir::constant_value(m, c).ok()?.bits(),
        _ => None,
    }
}

/// Constant branches, and branches whose jumps are all the same.
fn fold_terminators(m: &Module, f: &mut Function) -> bool {
    let mut changed = false;
    for b in f.layout.clone() {
        let Some(t) = f.terminator(b) else { continue };
        match f.insts[t].op.clone() {
            Op::CondBr { .. } => {
                let js = jumps(f, t);
                let cond = f.operands(t)[0];
                let pick = match const_bits(m, f, cond) {
                    Some(bits) => Some(if bits & 1 == 1 { 0 } else { 1 }),
                    None => (js[0] == js[1]).then_some(0),
                };
                if let Some(k) = pick {
                    let (target, args) = js[k].clone();
                    set_br(f, t, target, args);
                    changed = true;
                }
            }
            Op::Switch { cases, .. } if !cases.is_empty() => {
                let js = jumps(f, t);
                let default = js.last().cloned().expect("a default jump");
                let scrutinee = f.operands(t)[0];
                let taken = match const_bits(m, f, scrutinee) {
                    Some(bits) => Some(
                        cases
                            .iter()
                            .position(|c| c.value == bits)
                            .map_or(default.clone(), |k| js[k].clone()),
                    ),
                    None => js.iter().all(|j| *j == default).then(|| default.clone()),
                };
                if let Some(jump) = taken {
                    // Only the default jump is left, to where the switch goes.
                    let Op::Switch { merge, .. } = f.insts[t].op.clone() else {
                        unreachable!()
                    };
                    let (target, args) = jump;
                    let mut operands = vec![scrutinee];
                    let default_args = args.len() as u32;
                    operands.extend(args);
                    f.replace_inst(
                        t,
                        Op::Switch {
                            cases: Vec::new(),
                            default: target,
                            default_args,
                            merge,
                        },
                        operands,
                    );
                    changed = true;
                }
            }
            _ => {}
        }
    }
    changed
}

/// Deletes blocks the entry does not reach; empties the merge and continue blocks of reachable
/// regions among them.
pub(crate) fn remove_unreachable(f: &mut Function, cfg: &Cfg, st: &Structure) -> bool {
    let keep: HashSet<usize> = st
        .regions
        .iter()
        .filter(|r| cfg.reachable(r.owner))
        .flat_map(|r| [Some(r.merge), r.cont])
        .flatten()
        .collect();
    let mut dead = Vec::new();
    let mut stubs = Vec::new();
    for (i, &b) in cfg.blocks.iter().enumerate() {
        if cfg.reachable(i) {
            continue;
        }
        if keep.contains(&i) {
            let is_stub = f.blocks[b].params.is_empty()
                && f.insts(b).count() == 1
                && f.terminator(b)
                    .is_some_and(|t| f.insts[t].op == Op::Unreachable);
            if !is_stub {
                stubs.push(b);
            }
        } else {
            dead.push(b);
        }
    }
    if dead.is_empty() && stubs.is_empty() {
        return false;
    }
    let insts: Vec<_> = dead
        .iter()
        .chain(&stubs)
        .flat_map(|&b| f.insts(b).collect::<Vec<_>>())
        .collect();
    f.remove_insts(&insts);
    for &b in &stubs {
        for i in (0..f.blocks[b].params.len()).rev() {
            f.remove_param(b, i);
        }
        let u = f.create_inst(Op::Unreachable, Vec::new(), &[]);
        f.append_inst(b, u);
    }
    let dead: HashSet<Block> = dead.into_iter().collect();
    f.layout.retain(|b| !dead.contains(b));
    true
}

/// Whether every jump to `target` from a reachable block comes from the region `r` itself, not
/// from a region nested in it (a break out of it).
fn only_direct_jumps(cfg: &Cfg, st: &Structure, r: usize, target: usize) -> bool {
    cfg.preds[target]
        .iter()
        .all(|&p| st.innermost(p) == Some(r) || !st.regions[r].blocks[p])
}

/// Switches without cases and loops that never loop, once nothing breaks out of them.
fn degenerate_regions(f: &mut Function, cfg: &Cfg, st: &Structure) -> bool {
    for (ri, r) in st.regions.iter().enumerate() {
        if !cfg.reachable(r.owner) {
            continue;
        }
        let owner = cfg.blocks[r.owner];
        let Some(t) = f.terminator(owner) else {
            continue;
        };
        match (&f.insts[t].op, r.kind) {
            (Op::Switch { cases, .. }, RegionKind::Switch) if cases.is_empty() => {
                if only_direct_jumps(cfg, st, ri, r.merge) {
                    let (target, args) = jumps(f, t).remove(0);
                    set_br(f, t, target, args);
                    return true;
                }
            }
            (Op::Loop { .. }, RegionKind::Loop) => {
                let cont = r.cont.unwrap_or(r.header);
                let loops_back = cfg.preds[r.header].iter().any(|&p| r.blocks[p]);
                if !loops_back && !cfg.reachable(cont) && only_direct_jumps(cfg, st, ri, r.merge) {
                    let (target, args) = jumps(f, t).remove(0);
                    set_br(f, t, target, args);
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// `a: ... br b(args)` where only `a` jumps to `b`: `b`'s instructions move into `a`.
fn merge_chains(f: &mut Function, cfg: &Cfg, st: &Structure) -> bool {
    for (ai, &a) in cfg.blocks.iter().enumerate() {
        if !cfg.reachable(ai) {
            continue;
        }
        let Some(t) = f.terminator(a) else { continue };
        let Op::Br(b) = f.insts[t].op else { continue };
        let bi = cfg.index[&b];
        if bi == ai || bi == 0 || cfg.preds[bi] != [ai] || st.is_structural(bi) {
            continue;
        }
        let args = f.operands(t).to_vec();
        let params = f.blocks[b].params.clone();
        for (&p, &arg) in params.iter().zip(&args) {
            f.replace_all_uses(p, arg);
        }
        f.remove_inst(t);
        let moved: Vec<_> = f.insts(b).collect();
        for inst in moved {
            f.detach(inst);
            f.append_inst(a, inst);
        }
        for i in (0..params.len()).rev() {
            f.remove_param(b, i);
        }
        f.layout.retain(|&x| x != b);
        return true;
    }
    false
}

/// `b: br c(args)` with nothing else in it: jumps to `b` go straight to `c`.
fn jump_over_empty(f: &mut Function, cfg: &Cfg, st: &Structure) -> bool {
    let mut changed = false;
    for (bi, &b) in cfg.blocks.iter().enumerate() {
        if bi == 0 || !cfg.reachable(bi) || st.is_structural(bi) || f.insts(b).count() != 1 {
            continue;
        }
        let Some(t) = f.terminator(b) else { continue };
        let Op::Br(c) = f.insts[t].op else { continue };
        if c == b {
            continue;
        }
        let through = f.operands(t).to_vec();
        let params = f.blocks[b].params.clone();
        for &pi in &cfg.preds[bi] {
            let p = cfg.blocks[pi];
            let Some(pt) = f.terminator(p) else { continue };
            let js = jumps(f, pt);
            if matches!(f.insts[pt].op, Op::Loop { .. }) || js.iter().any(|(x, _)| *x == c) {
                continue;
            }
            let new = js
                .into_iter()
                .map(|(target, args)| {
                    if target != b {
                        return (target, args);
                    }
                    let mapped = through
                        .iter()
                        .map(|&v| match params.iter().position(|&q| q == v) {
                            Some(k) => args[k],
                            None => v,
                        })
                        .collect();
                    (c, mapped)
                })
                .collect();
            set_jumps(f, pt, new);
            changed = true;
        }
        if changed {
            return true;
        }
    }
    false
}
