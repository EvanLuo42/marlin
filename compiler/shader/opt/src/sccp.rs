//! Sparse conditional constant propagation (0005 §4.4 pass 2.1, Wegman and Zadeck): values
//! start unknown, blocks unreached; only jumps whose branch can go that way make a block
//! reached, and a block parameter is the meet of what reached jumps pass it. Operations are
//! evaluated with the interpreter's own semantics ([`evaluate_op`]), so folding agrees with
//! compile-time evaluation and with the interpreter that differential tests compare against.
//!
//! Values proven constant become constants; branches on them are left for [`SimplifyCfg`] to
//! fold.
//!
//! [`SimplifyCfg`]: crate::SimplifyCfg

use std::collections::{HashMap, HashSet};

use marlin_shaderc_ir::{
    Block, FuncId, Function, Module, Op, Val, Value, ValueDef, constant_value, evaluate_op,
    value_constant,
};

use crate::edit::{is_foldable, jumps};
use crate::{Analyses, Changed, FunctionPass};

pub struct Sccp;

impl FunctionPass for Sccp {
    fn name(&self) -> &'static str {
        "sccp"
    }

    fn run_function(
        &self,
        m: &mut Module,
        _: FuncId,
        f: &mut Function,
        _: &mut Analyses,
    ) -> Changed {
        if run(m, f) {
            Changed::Values
        } else {
            Changed::No
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Lat {
    /// Not known yet (nothing reached defines it).
    Top,
    Const(Val),
    /// Not a constant.
    Bottom,
}

fn meet(a: &Lat, b: &Lat) -> Lat {
    match (a, b) {
        (Lat::Top, x) | (x, Lat::Top) => x.clone(),
        (Lat::Const(x), Lat::Const(y)) if x == y => Lat::Const(x.clone()),
        _ => Lat::Bottom,
    }
}

struct Solver<'a> {
    m: &'a Module,
    f: &'a Function,
    lat: HashMap<Value, Lat>,
    reached: HashSet<Block>,
    /// Reached jumps: the block they leave and their position among its jumps.
    edges: HashSet<(Block, usize)>,
    /// For each block, the jumps into it: the block they leave, the position, the arguments.
    into: HashMap<Block, Vec<(Block, usize, Vec<Value>)>>,
}

impl Solver<'_> {
    fn get(&self, v: Value) -> Lat {
        match self.f.values[v].def {
            ValueDef::Const(c) => match constant_value(self.m, c) {
                Ok(val) => Lat::Const(val),
                Err(_) => Lat::Bottom,
            },
            ValueDef::Global(_) => Lat::Bottom,
            _ => self.lat.get(&v).cloned().unwrap_or(Lat::Top),
        }
    }

    /// Lowers `v` to `new` (never raises it); whether it changed.
    fn set(&mut self, v: Value, new: Lat) -> bool {
        let old = self.get(v);
        let merged = meet(&old, &new);
        if merged != old {
            self.lat.insert(v, merged);
            true
        } else {
            false
        }
    }

    fn eval(&self, inst: marlin_shaderc_ir::Inst) -> Lat {
        let f = self.f;
        let data = &f.insts[inst];
        if !is_foldable(&data.op) || data.results.len() != 1 {
            return Lat::Bottom;
        }
        let ops = f.operands(inst);
        let lats: Vec<Lat> = ops.iter().map(|&v| self.get(v)).collect();
        // `select` on a known condition is the chosen operand, whatever the other one is.
        if data.op == Op::Select
            && let Lat::Const(Val::Bits(c)) = &lats[0]
        {
            return lats[if c & 1 == 1 { 1 } else { 2 }].clone();
        }
        if lats.contains(&Lat::Bottom) {
            return Lat::Bottom;
        }
        if lats.contains(&Lat::Top) {
            return Lat::Top;
        }
        let vals: Vec<Val> = lats
            .into_iter()
            .map(|l| match l {
                Lat::Const(v) => v,
                _ => unreachable!(),
            })
            .collect();
        let types: Vec<_> = ops.iter().map(|&v| f.ty(v)).collect();
        match evaluate_op(self.m, &data.op, &vals, &types, Some(f.ty(data.results[0]))) {
            Ok(v) => Lat::Const(v),
            Err(_) => Lat::Bottom,
        }
    }

    /// The jumps a reached block's terminator can take.
    fn feasible(&self, b: Block) -> Vec<usize> {
        let f = self.f;
        let Some(t) = f.terminator(b) else {
            return Vec::new();
        };
        let n = jumps(f, t).len();
        match &f.insts[t].op {
            Op::CondBr { .. } => match self.get(f.operands(t)[0]) {
                Lat::Top => Vec::new(),
                Lat::Const(Val::Bits(c)) => vec![if c & 1 == 1 { 0 } else { 1 }],
                _ => vec![0, 1],
            },
            Op::Switch { cases, .. } => match self.get(f.operands(t)[0]) {
                Lat::Top => Vec::new(),
                Lat::Const(Val::Bits(v)) => {
                    vec![
                        cases
                            .iter()
                            .position(|c| c.value == v)
                            .unwrap_or(cases.len()),
                    ]
                }
                _ => (0..n).collect(),
            },
            _ => (0..n).collect(),
        }
    }

    fn solve(&mut self) {
        let f = self.f;
        let entry = f.layout[0];
        self.reached.insert(entry);
        for &p in f.params() {
            self.lat.insert(p, Lat::Bottom);
        }
        // Lattice values only go down, so this ends; each sweep follows the layout.
        let mut changed = true;
        while changed {
            changed = false;
            for &b in &f.layout {
                if !self.reached.contains(&b) {
                    continue;
                }
                if b != entry {
                    for (i, &p) in f.blocks[b].params.iter().enumerate() {
                        let mut acc = Lat::Top;
                        for (from, k, args) in self.into.get(&b).into_iter().flatten() {
                            if self.edges.contains(&(*from, *k)) {
                                acc = meet(&acc, &self.get(args[i]));
                            }
                        }
                        changed |= self.set(p, acc);
                    }
                }
                for inst in f.insts(b) {
                    if f.insts[inst].op.is_terminator() {
                        continue;
                    }
                    let new = self.eval(inst);
                    for &r in &f.insts[inst].results {
                        let l = if f.insts[inst].results.len() == 1 {
                            new.clone()
                        } else {
                            Lat::Bottom
                        };
                        changed |= self.set(r, l);
                    }
                }
                let Some(t) = f.terminator(b) else { continue };
                let js = jumps(f, t);
                for k in self.feasible(b) {
                    if self.edges.insert((b, k)) {
                        changed = true;
                        self.reached.insert(js[k].0);
                    }
                }
            }
        }
    }
}

fn run(m: &mut Module, f: &mut Function) -> bool {
    if f.layout.is_empty() {
        return false;
    }
    let mut into: HashMap<Block, Vec<(Block, usize, Vec<Value>)>> = HashMap::new();
    for &b in &f.layout {
        if let Some(t) = f.terminator(b) {
            for (k, (target, args)) in jumps(f, t).into_iter().enumerate() {
                into.entry(target).or_default().push((b, k, args));
            }
        }
    }
    let constants: Vec<(Value, Val)> = {
        let mut s = Solver {
            m,
            f,
            lat: HashMap::new(),
            reached: HashSet::new(),
            edges: HashSet::new(),
            into,
        };
        s.solve();
        let mut out: Vec<(Value, Val)> = s
            .lat
            .into_iter()
            .filter_map(|(v, l)| match l {
                Lat::Const(val) if !f.uses(v).is_empty() => Some((v, val)),
                _ => None,
            })
            .collect();
        out.sort_by_key(|(v, _)| *v);
        out
    };
    let mut changed = false;
    for (v, val) in constants {
        let ty = f.ty(v);
        let Some(c) = value_constant(m, &val, ty) else {
            continue;
        };
        let cv = f.const_value(c, ty);
        f.replace_all_uses(v, cv);
        changed = true;
    }
    changed
}
