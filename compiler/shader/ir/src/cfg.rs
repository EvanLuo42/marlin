//! Control-flow facts the verifier and the passes share: jumps, predecessors, dominators and
//! the structured regions (0006 §5.1). Blocks are named by their position in the layout.

use std::collections::HashMap;

use crate::func::{Block, Function};
use crate::op::Op;

/// The jumps between a function's blocks (merge and continue blocks are not jumps).
#[derive(Clone, Debug)]
pub struct Cfg {
    /// The layout: position to block.
    pub blocks: Vec<Block>,
    pub index: HashMap<Block, usize>,
    pub succs: Vec<Vec<usize>>,
    /// From blocks the entry reaches only.
    pub preds: Vec<Vec<usize>>,
    /// Immediate dominators (the entry is its own); `None` for blocks the entry does not
    /// reach.
    pub idom: Vec<Option<usize>>,
}

impl Cfg {
    pub fn new(f: &Function) -> Cfg {
        let blocks = f.layout.clone();
        let index: HashMap<Block, usize> =
            blocks.iter().enumerate().map(|(i, &b)| (b, i)).collect();
        let succs: Vec<Vec<usize>> = blocks
            .iter()
            .map(|&b| match f.terminator(b) {
                Some(t) => f.insts[t]
                    .op
                    .successors(f.operands(t).len())
                    .into_iter()
                    .filter_map(|(s, _)| index.get(&s).copied())
                    .collect(),
                None => Vec::new(),
            })
            .collect();
        let n = blocks.len();
        let idom = if n == 0 {
            Vec::new()
        } else {
            dominators(n, 0, &succs)
        };
        let mut preds = vec![Vec::new(); n];
        for (b, ss) in succs.iter().enumerate() {
            if idom[b].is_none() {
                continue;
            }
            for &s in ss {
                preds[s].push(b);
            }
        }
        Cfg {
            blocks,
            index,
            succs,
            preds,
            idom,
        }
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn reachable(&self, b: usize) -> bool {
        self.idom.get(b).is_some_and(Option::is_some)
    }

    pub fn dominates(&self, a: usize, b: usize) -> bool {
        dominates(&self.idom, a, b)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RegionKind {
    Selection,
    Switch,
    Loop,
}

/// A structured region (0006 §5.1), by block positions in the layout.
#[derive(Clone, Debug)]
pub struct Region {
    pub kind: RegionKind,
    pub header: usize,
    pub merge: usize,
    /// Loops: the continue block.
    pub cont: Option<usize>,
    /// The block whose terminator names the region: the header, or a loop's preheader.
    pub owner: usize,
    /// Which blocks are in the region.
    pub blocks: Vec<bool>,
}

/// A function's regions, outermost first (a loop before a selection with the same header),
/// and the dominators of the CFG with each header's edges to its merge and continue blocks
/// added.
#[derive(Clone, Debug)]
pub struct Structure {
    pub regions: Vec<Region>,
    pub sdom: Vec<Option<usize>>,
    /// The CFG with the structural edges.
    pub structural: Vec<Vec<usize>>,
}

impl Structure {
    /// The regions of `f`. Headers whose merge or continue blocks are malformed (the verifier
    /// reports them) are left out.
    pub fn new(f: &Function, cfg: &Cfg) -> Structure {
        let n = cfg.len();
        let mut regions = Vec::new();
        let mut structural = cfg.succs.clone();
        for (bi, &b) in cfg.blocks.iter().enumerate() {
            let Some(t) = f.terminator(b) else { continue };
            match &f.insts[t].op {
                Op::CondBr { merge, .. } | Op::Switch { merge, .. } => {
                    let kind = if matches!(f.insts[t].op, Op::Switch { .. }) {
                        RegionKind::Switch
                    } else {
                        RegionKind::Selection
                    };
                    let Some(&m) = cfg.index.get(merge) else {
                        continue;
                    };
                    if m == bi {
                        continue;
                    }
                    structural[bi].push(m);
                    regions.push(Region {
                        kind,
                        header: bi,
                        merge: m,
                        cont: None,
                        owner: bi,
                        blocks: Vec::new(),
                    });
                }
                Op::Loop {
                    header,
                    continuing,
                    merge,
                } => {
                    let (Some(&h), Some(&c), Some(&m)) = (
                        cfg.index.get(header),
                        cfg.index.get(continuing),
                        cfg.index.get(merge),
                    ) else {
                        continue;
                    };
                    if m == h || c == m {
                        continue;
                    }
                    structural[h].push(m);
                    structural[h].push(c);
                    regions.push(Region {
                        kind: RegionKind::Loop,
                        header: h,
                        merge: m,
                        cont: Some(c),
                        owner: bi,
                        blocks: Vec::new(),
                    });
                }
                _ => {}
            }
        }
        let sdom = if n == 0 {
            Vec::new()
        } else {
            dominators(n, 0, &structural)
        };
        let depth = |mut b: usize| {
            let mut d = 0;
            while let Some(p) = sdom[b].filter(|&p| p != b) {
                b = p;
                d += 1;
            }
            d
        };
        // Region blocks, outermost first. A region inside a loop also stops at the loop's
        // continue block (unless it is in the continue block's part of the loop), as SPIR-V's
        // constructs do.
        regions.sort_by_key(|r| (depth(r.header), r.kind != RegionKind::Loop));
        let mut innermost_loop: Vec<Option<usize>> = Vec::with_capacity(regions.len());
        for i in 0..regions.len() {
            let (header, merge) = (regions[i].header, regions[i].merge);
            let parent = (0..i)
                .filter(|&j| regions[j].blocks[header])
                .min_by_key(|&j| regions[j].blocks.iter().filter(|x| **x).count());
            let around = parent.and_then(|p| innermost_loop[p]);
            let stop = around
                .and_then(|l| regions[l].cont)
                .filter(|&c| !dominates(&sdom, c, header));
            let blocks: Vec<bool> = (0..n)
                .map(|b| {
                    sdom[b].is_some()
                        && dominates(&sdom, header, b)
                        && !dominates(&sdom, merge, b)
                        && parent.is_none_or(|p| regions[p].blocks[b])
                        && stop.is_none_or(|c| !dominates(&sdom, c, b))
                })
                .collect();
            regions[i].blocks = blocks;
            innermost_loop.push(if regions[i].kind == RegionKind::Loop {
                Some(i)
            } else {
                around
            });
        }
        Structure {
            regions,
            sdom,
            structural,
        }
    }

    /// The smallest region that holds block `b`.
    pub fn innermost(&self, b: usize) -> Option<usize> {
        self.regions
            .iter()
            .enumerate()
            .filter(|(_, r)| r.blocks.get(b).copied().unwrap_or(false))
            .min_by_key(|(_, r)| r.blocks.iter().filter(|x| **x).count())
            .map(|(i, _)| i)
    }

    /// Whether `b` is a merge or continue block, or a loop header.
    pub fn is_structural(&self, b: usize) -> bool {
        self.regions.iter().any(|r| {
            r.merge == b || r.cont == Some(b) || (r.kind == RegionKind::Loop && r.header == b)
        })
    }
}

/// Dominators of a graph over `0..n` from `entry`: `idom[b]` (the entry is its own), `None`
/// for blocks the entry does not reach.
pub fn dominators(n: usize, entry: usize, succs: &[Vec<usize>]) -> Vec<Option<usize>> {
    // Reverse postorder.
    let mut order = Vec::new();
    let mut seen = vec![false; n];
    let mut stack = vec![(entry, 0usize)];
    seen[entry] = true;
    while let Some((b, i)) = stack.pop() {
        if let Some(&s) = succs[b].get(i) {
            stack.push((b, i + 1));
            if !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            order.push(b);
        }
    }
    order.reverse();
    let mut rpo = vec![usize::MAX; n];
    for (i, &b) in order.iter().enumerate() {
        rpo[b] = i;
    }
    let mut preds = vec![Vec::new(); n];
    for &b in &order {
        for &s in &succs[b] {
            preds[s].push(b);
        }
    }
    // Cooper, Harvey and Kennedy's iteration.
    let mut idom: Vec<Option<usize>> = vec![None; n];
    idom[entry] = Some(entry);
    let mut changed = true;
    while changed {
        changed = false;
        for &b in order.iter().skip(1) {
            let mut new: Option<usize> = None;
            for &p in &preds[b] {
                if idom[p].is_none() {
                    continue;
                }
                new = Some(match new {
                    None => p,
                    Some(mut a) => {
                        let mut c = p;
                        while a != c {
                            while rpo[a] > rpo[c] {
                                a = idom[a].unwrap_or(entry);
                            }
                            while rpo[c] > rpo[a] {
                                c = idom[c].unwrap_or(entry);
                            }
                        }
                        a
                    }
                });
            }
            if new.is_some() && idom[b] != new {
                idom[b] = new;
                changed = true;
            }
        }
    }
    idom
}

/// Whether `a` dominates `b` (both reached).
pub fn dominates(idom: &[Option<usize>], a: usize, mut b: usize) -> bool {
    if idom[b].is_none() {
        return false;
    }
    loop {
        if a == b {
            return true;
        }
        match idom[b] {
            Some(p) if p != b => b = p,
            _ => return false,
        }
    }
}
