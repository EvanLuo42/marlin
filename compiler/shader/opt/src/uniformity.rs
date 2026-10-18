//! Uniformity (divergence) analysis (0005 §4.3): which values may differ between the lanes of
//! a wave or of a thread group, and which blocks may run with only some of the lanes.
//!
//! - Values: parameters, per-lane inputs (thread ids, vertex ids...), atomics, loads through
//!   `function` variables, `assume.nonuniform` and wave operations other than reductions,
//!   broadcasts, votes and ballots (all of them, for a whole group) differ; `assume.uniform`
//!   does not; anything else differs when an operand does.
//! - Control: a block runs with only some lanes when it depends (by post-dominance) on a branch
//!   whose condition differs, or on a block that does. A `return` under such a branch leaves
//!   the rest of the function to some lanes too.
//! - Joins: where the paths out of a region whose branch differs meet (its merge block, the
//!   loop merge or continue block it breaks or continues to), block parameters differ. A loop
//!   that lanes may leave at different times makes the values it defines differ, seen after it.
//!
//! Loads through other pointers that are the same in every lane are the same: the language has
//! no data races (writes another lane makes are seen after a barrier).

use std::collections::{HashMap, HashSet};

use marlin_shaderc_ir::cfg::{Cfg, RegionKind, Structure};
use marlin_shaderc_ir::{
    Binding, Block, Builtin, Function, Inst, Module, Op, Space, TypeData, Value, ValueDef, WaveOp,
};

use crate::edit::jumps;

/// Uniform across what.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    Wave,
    Group,
}

#[derive(Clone, Debug, Default)]
pub struct Uniformity {
    /// Values that may differ between lanes.
    pub divergent: HashSet<Value>,
    /// Blocks that may run with only some of the lanes, with the branch (a terminator whose
    /// condition differs) that makes it so.
    pub divergent_blocks: HashMap<Block, Inst>,
}

impl Uniformity {
    pub fn is_divergent(&self, v: Value) -> bool {
        self.divergent.contains(&v)
    }
}

/// Whether a value made by `inst` differs between lanes regardless of its operands.
fn source(m: &Module, f: &Function, inst: Inst, level: Level) -> Option<bool> {
    let ops = f.operands(inst);
    Some(match &f.insts[inst].op {
        Op::AssumeUniform => return Some(false),
        Op::AssumeNonUniform
        | Op::Atomic { .. }
        | Op::Call(_)
        | Op::Deriv { .. }
        | Op::IsHelper => true,
        Op::Wave { op, .. } => match level {
            Level::Group => !matches!(op, WaveOp::LaneCount),
            Level::Wave => matches!(
                op,
                WaveOp::Scan { .. }
                    | WaveOp::Shuffle
                    | WaveOp::Quad(_)
                    | WaveOp::LaneIndex
                    | WaveOp::IsFirst
                    | WaveOp::Match
            ),
        },
        Op::Load { .. } => return load_differs(m, f, ops[0]),
        _ => return None,
    })
}

/// A load through `ptr`: per-lane memory differs; other memory is the same where the pointer is
/// (`None`: decided by the operands).
fn load_differs(m: &Module, f: &Function, mut ptr: Value) -> Option<bool> {
    // The variable the pointer starts from.
    loop {
        match f.values[ptr].def {
            ValueDef::Result(i, _) if matches!(f.insts[i].op, Op::Access(_)) => {
                ptr = f.operands(i)[0]
            }
            _ => break,
        }
    }
    let TypeData::Ptr { space, .. } = m.data(f.ty(ptr)) else {
        return Some(true);
    };
    match space {
        Space::Function => Some(true),
        Space::Input => match f.values[ptr].def {
            // The group id is the same in every lane of the group.
            ValueDef::Global(g) => Some(!matches!(
                m.globals[g].binding,
                Binding::Builtin(Builtin::GroupId)
            )),
            _ => Some(true),
        },
        Space::Output | Space::MeshVertex | Space::MeshPrimitive | Space::MeshIndex => Some(true),
        _ => None,
    }
}

/// The analysis of `f` at `level`.
pub fn analyze(m: &Module, f: &Function, level: Level) -> Uniformity {
    let mut u = Uniformity::default();
    if f.layout.is_empty() {
        return u;
    }
    let cfg = Cfg::new(f);
    let st = Structure::new(f, &cfg);
    let deps = control_dependence(f, &cfg);
    let insts: Vec<Inst> = f
        .layout
        .iter()
        .flat_map(|&b| f.insts(b).collect::<Vec<_>>())
        .collect();
    // Jumps into each block.
    let mut into: HashMap<Block, Vec<(Block, Vec<Value>)>> = HashMap::new();
    for &b in &f.layout {
        if let Some(t) = f.terminator(b) {
            for (target, args) in jumps(f, t) {
                into.entry(target).or_default().push((b, args));
            }
        }
    }
    u.divergent.extend(f.params().iter().copied());
    let mut joins: HashSet<Block> = HashSet::new();
    let mut temporal: HashSet<usize> = HashSet::new(); // loops whose values differ after them
    let mut changed = true;
    while changed {
        changed = false;
        // Values.
        for &i in &insts {
            let data = &f.insts[i];
            let Some(&r) = data.results.first() else {
                continue;
            };
            if u.divergent.contains(&r) {
                continue;
            }
            let d = match source(m, f, i, level) {
                Some(d) => d,
                None => f.operands(i).iter().any(|v| u.divergent.contains(v)),
            };
            if d {
                for &r in &data.results {
                    u.divergent.insert(r);
                }
                changed = true;
            }
        }
        for &b in &f.layout {
            let join = joins.contains(&b);
            for (k, &p) in f.blocks[b].params.iter().enumerate() {
                if u.divergent.contains(&p) || b == f.layout[0] {
                    continue;
                }
                let d = join
                    || into
                        .get(&b)
                        .into_iter()
                        .flatten()
                        .any(|(_, args)| args.get(k).is_some_and(|a| u.divergent.contains(a)));
                if d {
                    u.divergent.insert(p);
                    changed = true;
                }
            }
        }
        // Branches whose condition differs: the blocks that depend on them, and their joins.
        for (bi, &b) in cfg.blocks.iter().enumerate() {
            let Some(t) = f.terminator(b) else { continue };
            let differs = match f.insts[t].op {
                Op::CondBr { .. } | Op::Switch { .. } => u.divergent.contains(&f.operands(t)[0]),
                _ => false,
            };
            let under = u.divergent_blocks.get(&b).copied();
            if !differs && under.is_none() {
                continue;
            }
            let cause = if differs { t } else { under.unwrap_or(t) };
            for &d in deps.get(&bi).into_iter().flatten() {
                if let std::collections::hash_map::Entry::Vacant(e) =
                    u.divergent_blocks.entry(cfg.blocks[d])
                {
                    e.insert(cause);
                    changed = true;
                }
            }
            if differs {
                for r in st
                    .regions
                    .iter()
                    .filter(|r| r.header == bi && r.kind != RegionKind::Loop)
                {
                    for x in exits(&cfg, r) {
                        changed |= joins.insert(cfg.blocks[x]);
                    }
                }
            }
        }
        // Loops that lanes may leave at different times.
        for (ri, r) in st.regions.iter().enumerate() {
            if r.kind != RegionKind::Loop || temporal.contains(&ri) {
                continue;
            }
            let leaves = (0..cfg.len()).filter(|&b| r.blocks[b]).any(|b| {
                let block = cfg.blocks[b];
                let exits_here = cfg.succs[b].iter().any(|&s| !r.blocks[s])
                    || f.terminator(block)
                        .is_some_and(|t| f.insts[t].op == Op::Return);
                exits_here
                    && (u.divergent_blocks.contains_key(&block)
                        || f.terminator(block).is_some_and(|t| {
                            matches!(f.insts[t].op, Op::CondBr { .. } | Op::Switch { .. })
                                && u.divergent.contains(&f.operands(t)[0])
                        }))
            });
            if leaves {
                temporal.insert(ri);
                changed = true;
                for x in exits(&cfg, r) {
                    joins.insert(cfg.blocks[x]);
                }
                for b in (0..cfg.len()).filter(|&b| r.blocks[b]) {
                    let block = cfg.blocks[b];
                    u.divergent.extend(f.blocks[block].params.iter().copied());
                    for i in f.insts(block) {
                        u.divergent.extend(f.insts[i].results.iter().copied());
                    }
                }
            }
        }
    }
    u
}

/// The blocks outside region `r` that jumps from inside it go to.
fn exits(cfg: &Cfg, r: &marlin_shaderc_ir::cfg::Region) -> Vec<usize> {
    let mut out: Vec<usize> = vec![r.merge];
    for b in (0..cfg.len()).filter(|&b| r.blocks[b]) {
        for &s in &cfg.succs[b] {
            if !r.blocks[s] && !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

/// For each block with a branch, the blocks that depend on which way it goes (Ferrante,
/// Ottenstein and Warren): those on a path from one of its successors up the post-dominator
/// tree, short of its own immediate post-dominator. Blocks that never return post-dominate
/// nothing.
fn control_dependence(f: &Function, cfg: &Cfg) -> HashMap<usize, Vec<usize>> {
    let n = cfg.len();
    // The reverse graph, with an exit node `n` after every block that returns or stops.
    let exit = n;
    let mut rsuccs = vec![Vec::new(); n + 1];
    for b in 0..n {
        for &s in &cfg.succs[b] {
            rsuccs[s].push(b);
        }
        let ends = f
            .terminator(cfg.blocks[b])
            .is_some_and(|t| matches!(f.insts[t].op, Op::Return | Op::Unreachable));
        if ends || cfg.succs[b].is_empty() {
            rsuccs[exit].push(b);
        }
    }
    let mut ipdom = marlin_shaderc_ir::cfg::dominators(n + 1, exit, &rsuccs);
    // Blocks that cannot reach the exit (endless loops): hang them off the exit.
    for p in ipdom.iter_mut().take(n) {
        if p.is_none() {
            *p = Some(exit);
        }
    }
    let mut deps: HashMap<usize, Vec<usize>> = HashMap::new();
    for x in 0..n {
        if !cfg.reachable(x) || cfg.succs[x].len() < 2 {
            continue;
        }
        let stop = ipdom[x];
        for &s in &cfg.succs[x] {
            let mut at = Some(s);
            while let Some(y) = at {
                if Some(y) == stop || y == exit {
                    break;
                }
                let list = deps.entry(x).or_default();
                if !list.contains(&y) {
                    list.push(y);
                }
                at = ipdom[y].filter(|&p| p != y);
            }
        }
    }
    deps
}
