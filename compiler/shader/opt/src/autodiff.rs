//! Forward-mode automatic differentiation (0004 §15, 0005 §4.4 pass 0.3; decided 2026-10-18).
//!
//! Lowering marks what to differentiate, and this pass, after inlining, SROA and mem2reg,
//! turns the marks into arithmetic on tangents in the one function left:
//! - `ForwardDiff(f)(DiffPair(x, dx), ...)` becomes a tangent *channel* of its own
//!   (`diff.channel`): `diff.seed %c, %x, %dx` gives `x` the tangent `dx` in it, and
//!   `diff.tangent %c, %r` reads the tangent of the call's result.
//! - `Derivatives.Seed(x, dx, dy)` (`deriv.seed`) gives `x` tangents in two more channels, the
//!   screen's x and y, which `deriv.ddx`, `deriv.ddy` and samples with implicit levels read.
//!   A pixel entry that seeds nothing keeps them for the hardware; anything else gets them
//!   from the seeds (0 where nothing is seeded).
//! - `diff.detach` (`Detach`, `[NoDiff]` arguments) has no tangent in any channel.
//!
//! For each channel: the values a seed reaches through differentiable operations are
//! *active*, the values a query reads, back through the operations computing them, are
//! *useful*, and only values both active and useful get a tangent, emitted right after them.
//! Block parameters get tangent parameters, and `function` variables a shadow variable for
//! the tangents stored in them. `ForwardDiff` channels go first, inner ones first, so the
//! channels after them differentiate their tangent code too (second derivatives); a
//! screen-space derivative of a screen-space derivative is 0, as a coarse one is on the
//! hardware.
//!
//! Operations without a derivative (texture samples, wave operations, atomics, bitcasts, a
//! derivative another channel has yet to resolve) take it as 0, with a warning (E1402) where a
//! query needs the derivative through one. Operations that are flat almost everywhere
//! (`floor`, `step`, comparisons, conversions to integers, memory read at an index) are 0
//! without one.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::f64::consts::LN_2;

use marlin_shaderc_ir::cfg::Cfg;
use marlin_shaderc_ir::{
    Axis, BinaryOp, Block, ConstValue, ConvertOp, FloatCmp, FuncId, Function, Inst, MathOp, Module,
    Op, Scalar, Stage, TexArg, Type, TypeData, UnaryOp, Value, ValueDef,
};

use crate::edit::{jumps, set_jumps, value_before, zero};
use crate::{Analyses, Changed, Pass, Problem};

/// The pass. It keeps the warnings of its last run for [`Pass::problems`].
#[derive(Default)]
pub struct Autodiff {
    warnings: RefCell<Vec<Problem>>,
}

impl Pass for Autodiff {
    fn name(&self) -> &'static str {
        "autodiff"
    }

    fn run(&self, m: &mut Module, analyses: &mut Analyses) -> bool {
        let ids: Vec<FuncId> = m.funcs.iter().map(|(id, _)| id).collect();
        let mut warnings = Vec::new();
        let mut any = false;
        for id in ids {
            if !marked(&m.funcs[id]) {
                continue;
            }
            let pixel = m
                .entries
                .iter()
                .any(|e| e.func == id && e.stage == Stage::Pixel);
            let mut f = std::mem::replace(&mut m.funcs[id], Function::new("", Vec::new()));
            let hardware = pixel && !has_op(&f, |op| matches!(op, Op::DerivSeed));
            differentiate(m, &mut f, hardware, &mut warnings);
            m.funcs[id] = f;
            analyses.invalidate(id, Changed::Cfg);
            any = true;
        }
        *self.warnings.borrow_mut() = warnings;
        any
    }

    fn problems(&self, _: &Module) -> Vec<Problem> {
        std::mem::take(&mut *self.warnings.borrow_mut())
    }
}

/// Whether `f` has anything for the pass to do.
fn marked(f: &Function) -> bool {
    has_op(f, |op| {
        matches!(
            op,
            Op::DiffChannel | Op::DiffDetach | Op::DerivSeed | Op::Deriv { .. }
        ) || implicit_sample(op)
    })
}

fn has_op(f: &Function, pred: impl Fn(&Op) -> bool) -> bool {
    f.layout
        .iter()
        .any(|&b| f.insts(b).any(|i| pred(&f.insts[i].op)))
}

/// A sample whose level comes from derivatives.
fn implicit_sample(op: &Op) -> bool {
    matches!(op, Op::TexSample(a) if !a.contains(&TexArg::Lod) && !a.contains(&TexArg::Grad))
}

fn placed(f: &Function) -> Vec<Inst> {
    f.layout.iter().flat_map(|&b| f.insts(b)).collect()
}

/// A tangent channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dir {
    /// One `ForwardDiff` call: its `diff.channel`.
    Forward(Value),
    /// The screen's x.
    X,
    /// The screen's y.
    Y,
}

impl Dir {
    fn prefix(self) -> &'static str {
        match self {
            Dir::Forward(_) => "d_",
            Dir::X => "ddx_",
            Dir::Y => "ddy_",
        }
    }
}

fn differentiate(m: &mut Module, f: &mut Function, hardware: bool, warnings: &mut Vec<Problem>) {
    // `ForwardDiff` channels, a channel after those whose tangents its queries read.
    let mut channels: Vec<Value> = placed(f)
        .into_iter()
        .filter(|&i| f.insts[i].op == Op::DiffChannel)
        .map(|i| f.result(i))
        .collect();
    while !channels.is_empty() {
        let pick = (0..channels.len())
            .find(|&k| {
                let deps = channel_deps(f, channels[k]);
                channels
                    .iter()
                    .all(|c| *c == channels[k] || !deps.contains(c))
            })
            .unwrap_or(0);
        let c = channels.remove(pick);
        let t = tangents(m, f, Dir::Forward(c), warnings);
        for inst in placed(f) {
            let ops = f.operands(inst).to_vec();
            if f.insts[inst].op == Op::DiffTangent && ops[0] == c {
                let tv = tangent_or_zero(m, f, &t, ops[1]);
                let r = f.result(inst);
                f.replace_all_uses(r, tv);
                f.remove_inst(inst);
            }
        }
        unwrap(f, |op, ops| *op == Op::DiffSeed && ops[0] == c, 1);
        if let ValueDef::Result(inst, _) = f.values[c].def
            && f.uses(c).is_empty()
        {
            f.remove_inst(inst);
        }
    }

    if !hardware {
        let tx = tangents(m, f, Dir::X, warnings);
        let ty = tangents(m, f, Dir::Y, warnings);
        for inst in placed(f) {
            let op = f.insts[inst].op.clone();
            match op {
                Op::Deriv { axis, .. } => {
                    let v = f.operands(inst)[0];
                    let t = if axis == Axis::X { &tx } else { &ty };
                    let tv = tangent_or_zero(m, f, t, v);
                    let r = f.result(inst);
                    f.replace_all_uses(r, tv);
                    f.remove_inst(inst);
                }
                Op::TexSample(args) if implicit_sample(&f.insts[inst].op) => {
                    explicit_sample(m, f, inst, &args, &tx, &ty);
                }
                _ => {}
            }
        }
        unwrap(f, |op, _| *op == Op::DerivSeed, 0);
    }
    unwrap(f, |op, _| *op == Op::DiffDetach, 0);
}

/// Replaces the marks `which` accepts by their operand `keep`.
fn unwrap(f: &mut Function, which: impl Fn(&Op, &[Value]) -> bool, keep: usize) {
    for inst in placed(f) {
        if which(&f.insts[inst].op, f.operands(inst)) {
            let (r, v) = (f.result(inst), f.operands(inst)[keep]);
            f.replace_all_uses(r, v);
            f.remove_inst(inst);
        }
    }
}

/// The other channels whose tangents the queries of channel `c` read, directly or not.
fn channel_deps(f: &Function, c: Value) -> HashSet<Value> {
    let mem = Memory::new(f);
    let mut starts = Vec::new();
    for inst in placed(f) {
        if f.insts[inst].op == Op::DiffTangent && f.operands(inst)[0] == c {
            starts.push(f.operands(inst)[1]);
        }
    }
    let mut seen = HashSet::new();
    let mut deps = HashSet::new();
    while let Some(v) = starts.pop() {
        if !seen.insert(v) {
            continue;
        }
        match f.values[v].def {
            ValueDef::Param(b, i) => {
                for &(term, start) in mem.incoming.get(&b).into_iter().flatten() {
                    starts.push(f.operands(term)[start + i as usize]);
                }
            }
            ValueDef::Result(inst, _) => {
                let ops = f.operands(inst);
                match f.insts[inst].op {
                    Op::DiffTangent => {
                        deps.insert(ops[0]);
                    }
                    Op::Load { .. } => {
                        if let Some(var) = root_var(f, ops[0]) {
                            for &s in &mem.of(var).1 {
                                starts.push(f.operands(s)[1]);
                            }
                        }
                    }
                    _ => {}
                }
                starts.extend(ops.iter().copied());
            }
            _ => {}
        }
    }
    deps
}

/// The tangent of `v` (0 when it has none).
fn tangent_or_zero(m: &mut Module, f: &mut Function, t: &HashMap<Value, Value>, v: Value) -> Value {
    match t.get(&v) {
        Some(&tv) => tv,
        None => {
            let ty = f.ty(v);
            zero(m, f, ty)
        }
    }
}

/// Replaces a sample with an implicit level by one with explicit gradients: the coordinate's
/// tangents along x and y, scaled by `2^bias` for a `bias`; `lod: 0` when both are 0.
fn explicit_sample(
    m: &mut Module,
    f: &mut Function,
    inst: Inst,
    args: &[TexArg],
    tx: &HashMap<Value, Value>,
    ty: &HashMap<Value, Value>,
) {
    let ops = f.operands(inst).to_vec();
    let coord = ops[2];
    let mut given: Vec<(TexArg, Vec<Value>)> = Vec::new();
    let mut at = 3;
    for &a in args {
        let n = if a == TexArg::Grad { 2 } else { 1 };
        given.push((a, ops[at..at + n].to_vec()));
        at += n;
    }
    let bias = given
        .iter()
        .find(|(a, _)| *a == TexArg::Bias)
        .map(|(_, v)| v[0]);
    let level = match (tx.get(&coord), ty.get(&coord)) {
        (None, None) => {
            let f32_ty = m.scalar(Scalar::F32);
            let c = m.float(f32_ty, 0.0);
            (TexArg::Lod, vec![f.const_value(c, f32_ty)])
        }
        (gx, gy) => {
            let mut e = Emit::new(m, f, inst, Dir::X);
            let ct = e.f.ty(coord);
            let mut gx = gx.copied().unwrap_or_else(|| e.zero(ct));
            let mut gy = gy.copied().unwrap_or_else(|| e.zero(ct));
            if let Some(b) = bias {
                let s = e.math(MathOp::Exp2, vec![b]);
                let s = e.widen(s, gx);
                gx = e.mul(gx, s);
                gy = e.mul(gy, s);
            }
            (TexArg::Grad, vec![gx, gy])
        }
    };
    let mut new_args = Vec::new();
    let mut operands = ops[..3].to_vec();
    for &a in TexArg::ALL {
        if a == level.0 {
            new_args.push(a);
            operands.extend(level.1.iter().copied());
        } else if a != TexArg::Bias
            && let Some((_, v)) = given.iter().find(|(g, _)| *g == a)
        {
            new_args.push(a);
            operands.extend(v.iter().copied());
        }
    }
    let r = f.result(inst);
    let rty = f.ty(r);
    let new = f.create_inst(Op::TexSample(new_args), operands, &[rty]);
    f.insts[new].loc = f.insts[inst].loc;
    f.insert_before(inst, new);
    let nr = f.result(new);
    f.replace_all_uses(r, nr);
    f.remove_inst(inst);
}

// ---- analysis ----

/// What an operation does to tangents.
enum Kind {
    /// The result's tangent comes from the tangents of these operands.
    Carry(Vec<usize>),
    /// The result's tangent is this operand (the channel's seed).
    Seed(usize),
    /// A `load`: the tangent is in the variable's shadow.
    Load,
    /// The derivative is 0 (or the result is not a number).
    Zero,
    /// No derivative: what it is, for the warning.
    Opaque(&'static str),
}

fn kind(f: &Function, inst: Inst, dir: Dir) -> Kind {
    let ops = f.operands(inst);
    let all = || Kind::Carry((0..ops.len()).collect());
    let first = || Kind::Carry(vec![0]);
    match &f.insts[inst].op {
        Op::Binary(b) if b.is_float() => all(),
        Op::Unary(UnaryOp::FNeg) => first(),
        Op::Select => Kind::Carry(vec![1, 2]),
        Op::Convert(ConvertOp::FpExt | ConvertOp::FpTrunc) => first(),
        Op::Convert(ConvertOp::Bitcast) => Kind::Opaque("a bitcast"),
        Op::Math(m) => match m {
            MathOp::Fma
            | MathOp::FMin
            | MathOp::FMax
            | MathOp::Dot
            | MathOp::Cross
            | MathOp::Pow
            | MathOp::Atan2
            | MathOp::Distance
            | MathOp::Reflect
            | MathOp::Refract
            | MathOp::Smoothstep => all(),
            MathOp::Sqrt
            | MathOp::Rsqrt
            | MathOp::Rcp
            | MathOp::Exp2
            | MathOp::Log2
            | MathOp::Sin
            | MathOp::Cos
            | MathOp::Abs
            | MathOp::Fract
            | MathOp::Tan
            | MathOp::Asin
            | MathOp::Acos
            | MathOp::Atan
            | MathOp::Sinh
            | MathOp::Cosh
            | MathOp::Tanh
            | MathOp::Exp
            | MathOp::Log
            | MathOp::Normalize
            | MathOp::Length
            | MathOp::FaceForward => first(),
            _ => Kind::Zero,
        },
        Op::Splat
        | Op::Extract(_)
        | Op::ExtractDyn
        | Op::Transpose
        | Op::MatColumn(_)
        | Op::MatRow(_)
        | Op::MatExtract(..)
        | Op::ExtractField(_)
        | Op::ExtractElem(_)
        | Op::AssumeUniform
        | Op::AssumeNonUniform => first(),
        Op::Insert(_) | Op::InsertField(_) | Op::InsertElem(_) | Op::Shuffle(_) | Op::MatMul => {
            Kind::Carry(vec![0, 1])
        }
        Op::InsertDyn => Kind::Carry(vec![0, 2]),
        Op::Construct => all(),
        Op::Load { .. } => Kind::Load,
        Op::DiffSeed => match dir {
            Dir::Forward(c) if ops[0] == c => Kind::Seed(2),
            _ => Kind::Carry(vec![1]),
        },
        Op::DerivSeed => match dir {
            Dir::Forward(_) => first(),
            Dir::X => Kind::Seed(1),
            Dir::Y => Kind::Seed(2),
        },
        Op::DiffTangent => match dir {
            Dir::Forward(c) if ops[0] == c => Kind::Zero,
            _ => Kind::Opaque("a derivative another derivative needs"),
        },
        Op::Deriv { .. } => match dir {
            Dir::Forward(_) => Kind::Opaque("a screen-space derivative"),
            _ => Kind::Zero,
        },
        Op::TexSample(_) | Op::TexGather { .. } => Kind::Opaque("a texture sample"),
        Op::Wave { .. } => Kind::Opaque("a wave operation"),
        Op::Atomic { .. } => Kind::Opaque("an atomic operation"),
        Op::Call(_) => Kind::Opaque("a call"),
        Op::RqGet { .. } => Kind::Opaque("a ray query"),
        _ => Kind::Zero,
    }
}

/// The values of channel `dir` that queries read.
fn queries(f: &Function, dir: Dir) -> Vec<Value> {
    let mut out = Vec::new();
    for inst in placed(f) {
        let ops = f.operands(inst);
        let op = &f.insts[inst].op;
        match (dir, op) {
            (Dir::Forward(c), Op::DiffTangent) if ops[0] == c => out.push(ops[1]),
            (Dir::X, Op::Deriv { axis: Axis::X, .. })
            | (Dir::Y, Op::Deriv { axis: Axis::Y, .. }) => out.push(ops[0]),
            (Dir::X | Dir::Y, _) if implicit_sample(op) => out.push(ops[2]),
            _ => {}
        }
    }
    out
}

/// Whether values of type `t` hold floating-point numbers somewhere.
fn has_floats(m: &Module, t: Type) -> bool {
    match m.data(t) {
        TypeData::Scalar(s) | TypeData::Vector(s, _) => s.is_float(),
        TypeData::Matrix { element, .. } => element.is_float(),
        TypeData::Array(e, _) => has_floats(m, *e),
        TypeData::Struct(s) => m.structs[*s].fields.iter().any(|(_, t)| has_floats(m, *t)),
        _ => false,
    }
}

/// The `function` variable a pointer points into.
fn root_var(f: &Function, mut p: Value) -> Option<Inst> {
    loop {
        let ValueDef::Result(inst, _) = f.values[p].def else {
            return None;
        };
        match f.insts[inst].op {
            Op::Var => return Some(inst),
            Op::Access(_) => p = f.operands(inst)[0],
            _ => return None,
        }
    }
}

/// The loads and stores of each variable, and the jumps into each block.
struct Memory {
    vars: HashMap<Inst, (Vec<Inst>, Vec<Inst>)>,
    /// Per block: each jump to it, as its terminator and where its arguments start.
    incoming: HashMap<Block, Vec<(Inst, usize)>>,
}

impl Memory {
    fn new(f: &Function) -> Memory {
        let mut vars: HashMap<Inst, (Vec<Inst>, Vec<Inst>)> = HashMap::new();
        let mut incoming: HashMap<Block, Vec<(Inst, usize)>> = HashMap::new();
        for inst in placed(f) {
            let op = &f.insts[inst].op;
            match op {
                Op::Load { .. } | Op::Store { .. } => {
                    if let Some(var) = root_var(f, f.operands(inst)[0]) {
                        let e = vars.entry(var).or_default();
                        if matches!(op, Op::Load { .. }) {
                            e.0.push(inst);
                        } else {
                            e.1.push(inst);
                        }
                    }
                }
                _ if op.is_terminator() => {
                    for (b, range) in op.successors(f.operands(inst).len()) {
                        incoming.entry(b).or_default().push((inst, range.start));
                    }
                }
                _ => {}
            }
        }
        Memory { vars, incoming }
    }

    fn of(&self, var: Inst) -> (Vec<Inst>, Vec<Inst>) {
        self.vars.get(&var).cloned().unwrap_or_default()
    }
}

/// Values reached forward from `starts`: through instructions where `step(inst, operand)`,
/// jumps to block parameters, and stores to variables (to their loads). Only values `keep`
/// accepts join. Also the variables reached.
fn spread(
    f: &Function,
    mem: &Memory,
    starts: Vec<Value>,
    step: &dyn Fn(Inst, usize) -> bool,
    keep: &dyn Fn(Value) -> bool,
) -> (HashSet<Value>, HashSet<Inst>) {
    let mut set = HashSet::new();
    let mut vars = HashSet::new();
    let mut work = Vec::new();
    let add = |v: Value, set: &mut HashSet<Value>, work: &mut Vec<Value>| {
        if keep(v) && set.insert(v) {
            work.push(v);
        }
    };
    for v in starts {
        add(v, &mut set, &mut work);
    }
    while let Some(v) = work.pop() {
        for u in f.uses(v).to_vec() {
            let inst = u.inst;
            let index = u.index as usize;
            let op = &f.insts[inst].op;
            if op.is_terminator() {
                for (b, range) in op.successors(f.operands(inst).len()) {
                    if range.contains(&index) {
                        let p = f.blocks[b].params[index - range.start];
                        add(p, &mut set, &mut work);
                    }
                }
            } else if matches!(op, Op::Store { .. }) {
                if index == 1
                    && let Some(var) = root_var(f, f.operands(inst)[0])
                    && vars.insert(var)
                {
                    for load in mem.of(var).0 {
                        add(f.result(load), &mut set, &mut work);
                    }
                }
            } else if step(inst, index) {
                for &r in &f.insts[inst].results {
                    add(r, &mut set, &mut work);
                }
            }
        }
    }
    (set, vars)
}

struct Facts {
    active: HashSet<Value>,
    useful: HashSet<Value>,
    shadowed: HashSet<Inst>,
}

impl Facts {
    fn needed(&self, v: Value) -> bool {
        self.active.contains(&v) && self.useful.contains(&v)
    }
}

fn analyze(m: &Module, f: &Function, dir: Dir, warnings: &mut Vec<Problem>) -> Facts {
    let mem = Memory::new(f);
    let mut seeds = Vec::new();
    for inst in placed(f) {
        if let Kind::Seed(_) = kind(f, inst, dir) {
            seeds.push(f.result(inst));
        }
    }
    let floats = |v: Value| has_floats(m, f.ty(v));
    let carries = |inst: Inst, index: usize| match kind(f, inst, dir) {
        Kind::Carry(pos) => pos.contains(&index),
        _ => false,
    };
    let (active, active_vars) = spread(f, &mem, seeds.clone(), &carries, &floats);
    let any = |inst: Inst, _: usize| f.insts[inst].op != Op::DiffDetach;
    let (tainted, _) = spread(f, &mem, seeds, &any, &|_| true);

    // Back from the queries, through the operations whose tangents they need.
    let mut useful = HashSet::new();
    let mut useful_vars = HashSet::new();
    let mut work = queries(f, dir);
    while let Some(v) = work.pop() {
        if !useful.insert(v) || !active.contains(&v) {
            continue;
        }
        match f.values[v].def {
            ValueDef::Param(b, i) => {
                for &(term, start) in mem.incoming.get(&b).into_iter().flatten() {
                    work.push(f.operands(term)[start + i as usize]);
                }
            }
            ValueDef::Result(inst, _) => match kind(f, inst, dir) {
                Kind::Carry(pos) => work.extend(pos.iter().map(|&p| f.operands(inst)[p])),
                Kind::Load => {
                    if let Some(var) = root_var(f, f.operands(inst)[0])
                        && useful_vars.insert(var)
                    {
                        for s in mem.of(var).1 {
                            work.push(f.operands(s)[1]);
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    // Derivatives a query needs through operations that have none.
    for &v in &useful {
        if active.contains(&v) || !floats(v) {
            continue;
        }
        let ValueDef::Result(inst, _) = f.values[v].def else {
            continue;
        };
        let Kind::Opaque(what) = kind(f, inst, dir) else {
            continue;
        };
        if !f.operands(inst).iter().any(|o| tainted.contains(o)) {
            continue;
        }
        let by = match dir {
            Dir::Forward(_) => "`ForwardDiff` takes",
            _ => "`Ddx`, `Ddy` and samples with an implicit level take",
        };
        let p = Problem {
            code: "E1402",
            message: format!("{what} has no derivative: {by} the derivative through it as 0"),
            at: f.insts[inst].loc,
            because: None,
            warning: true,
        };
        if !warnings.contains(&p) {
            warnings.push(p);
        }
    }

    let shadowed = active_vars.intersection(&useful_vars).copied().collect();
    Facts {
        active,
        useful,
        shadowed,
    }
}

// ---- emission ----

/// Emits the tangents of channel `dir`; returns them by value (values without one have 0).
fn tangents(
    m: &mut Module,
    f: &mut Function,
    dir: Dir,
    warnings: &mut Vec<Problem>,
) -> HashMap<Value, Value> {
    let facts = analyze(m, f, dir, warnings);
    let mut t: HashMap<Value, Value> = HashMap::new();
    if facts.useful.is_empty() || facts.active.is_empty() {
        return t;
    }
    // Tangent parameters, after the block's own.
    let mut params: HashMap<Block, Vec<usize>> = HashMap::new();
    for b in f.layout.clone() {
        for (i, p) in f.blocks[b].params.clone().into_iter().enumerate() {
            if facts.needed(p) {
                let name = tangent_name(f, p, dir);
                let ty = f.ty(p);
                let tp = f.add_param(b, ty, name.as_deref());
                t.insert(p, tp);
                params.entry(b).or_default().push(i);
            }
        }
    }
    // Instructions, definitions before uses.
    let mut shadow: HashMap<Value, Value> = HashMap::new();
    for b in reverse_postorder(f) {
        let insts: Vec<Inst> = f.insts(b).collect();
        for inst in insts {
            if f.insts[inst].op.is_terminator() {
                continue;
            }
            let at = f.next_inst(inst).expect("a terminator ends every block");
            let ops = f.operands(inst).to_vec();
            let op = f.insts[inst].op.clone();
            match op {
                Op::Var => {
                    if facts.shadowed.contains(&inst) {
                        let r = f.result(inst);
                        let ty = f.ty(r);
                        let mut e = Emit::new(m, f, at, dir);
                        e.loc = None;
                        let sv = e.op_ty(Op::Var, Vec::new(), Some(ty));
                        let TypeData::Ptr { pointee, .. } = *e.m.data(ty) else {
                            unreachable!("a variable is a pointer")
                        };
                        let z = e.zero(pointee);
                        e.store(sv, z);
                        let name = tangent_name(f, r, dir);
                        f.set_name(sv, name.as_deref());
                        shadow.insert(r, sv);
                    }
                }
                Op::Access(_) => {
                    if let Some(&sb) = shadow.get(&ops[0]) {
                        let mut o = ops.clone();
                        o[0] = sb;
                        let mut e = Emit::new(m, f, at, dir);
                        let s = e.op(op, o);
                        shadow.insert(f.result(inst), s);
                    }
                }
                Op::Store { .. } => {
                    if let Some(&sp) = shadow.get(&ops[0]) {
                        let tv = tangent_or_zero(m, f, &t, ops[1]);
                        Emit::new(m, f, at, dir).store(sp, tv);
                    }
                }
                _ => {
                    let Some(&r) = f.insts[inst].results.first() else {
                        continue;
                    };
                    if !facts.needed(r) {
                        continue;
                    }
                    let tv = match kind(f, inst, dir) {
                        Kind::Seed(i) => Some(ops[i]),
                        Kind::Load => shadow.get(&ops[0]).copied().map(|sp| {
                            Emit::new(m, f, at, dir).op(Op::Load { aligned: None }, vec![sp])
                        }),
                        Kind::Carry(pos) => {
                            let tans: Vec<Option<Value>> = ops
                                .iter()
                                .enumerate()
                                .map(|(i, o)| {
                                    if pos.contains(&i) {
                                        t.get(o).copied()
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            let mut e = Emit::new(m, f, at, dir);
                            e.loc = e.f.insts[inst].loc;
                            rule(&mut e, &op, &ops, r, &tans)
                        }
                        _ => None,
                    };
                    if let Some(tv) = tv {
                        if !matches!(f.values[tv].def, ValueDef::Const(_))
                            && f.values[tv].name.is_none()
                            && !ops.contains(&tv)
                        {
                            let name = tangent_name(f, r, dir);
                            f.set_name(tv, name.as_deref());
                        }
                        t.insert(r, tv);
                    }
                }
            }
        }
    }
    // Jumps pass the tangents of their arguments.
    for b in f.layout.clone() {
        let Some(term) = f.terminator(b) else {
            continue;
        };
        let js = jumps(f, term);
        if !js.iter().any(|(target, _)| params.contains_key(target)) {
            continue;
        }
        let mut new = Vec::new();
        for (target, args) in js {
            let mut all = args.clone();
            for &i in params.get(&target).into_iter().flatten() {
                all.push(tangent_or_zero(m, f, &t, args[i]));
            }
            new.push((target, all));
        }
        set_jumps(f, term, new);
    }
    t
}

fn tangent_name(f: &Function, v: Value, dir: Dir) -> Option<String> {
    f.values[v]
        .name
        .as_ref()
        .map(|n| format!("{}{n}", dir.prefix()))
}

/// The reachable blocks, each after its dominators.
fn reverse_postorder(f: &Function) -> Vec<Block> {
    let cfg = Cfg::new(f);
    if cfg.is_empty() {
        return Vec::new();
    }
    let mut seen = vec![false; cfg.len()];
    let mut post = Vec::new();
    let mut stack = vec![(0usize, 0usize)];
    seen[0] = true;
    while let Some((b, i)) = stack.pop() {
        if let Some(&s) = cfg.succs[b].get(i) {
            stack.push((b, i + 1));
            if !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            post.push(b);
        }
    }
    post.reverse();
    post.into_iter().map(|b| cfg.blocks[b]).collect()
}

/// Emits instructions before `at`.
struct Emit<'a> {
    m: &'a mut Module,
    f: &'a mut Function,
    at: Inst,
    loc: Option<u32>,
}

impl<'a> Emit<'a> {
    fn new(m: &'a mut Module, f: &'a mut Function, at: Inst, _: Dir) -> Emit<'a> {
        let loc = f.insts[at].loc;
        Emit { m, f, at, loc }
    }

    fn op_ty(&mut self, op: Op, operands: Vec<Value>, explicit: Option<Type>) -> Value {
        let v = value_before(self.m, self.f, self.at, op, operands, explicit);
        if let ValueDef::Result(i, _) = self.f.values[v].def {
            self.f.insts[i].loc = self.loc;
        }
        v
    }

    fn op(&mut self, op: Op, operands: Vec<Value>) -> Value {
        self.op_ty(op, operands, None)
    }

    fn store(&mut self, p: Value, v: Value) {
        let inst = self
            .f
            .create_inst(Op::Store { aligned: None }, vec![p, v], &[]);
        self.f.insts[inst].loc = self.loc;
        self.f.insert_before(self.at, inst);
    }

    fn zero(&mut self, t: Type) -> Value {
        zero(self.m, self.f, t)
    }

    /// `x` in every component of type `t`.
    fn konst(&mut self, t: Type, x: f64) -> Value {
        let c = float_const(self.m, t, x);
        self.f.const_value(c, t)
    }

    fn or_zero(&mut self, t: Option<Value>, like: Value) -> Value {
        match t {
            Some(t) => t,
            None => {
                let ty = self.f.ty(like);
                self.zero(ty)
            }
        }
    }

    /// `v` widened to the type of `like` when it is a scalar and `like` is not.
    fn widen(&mut self, v: Value, like: Value) -> Value {
        let (tv, tl) = (self.f.ty(v), self.f.ty(like));
        if tv == tl || !matches!(self.m.data(tv), TypeData::Scalar(_)) {
            return v;
        }
        self.op_ty(Op::Splat, vec![v], Some(tl))
    }

    fn bin(&mut self, op: BinaryOp, a: Value, b: Value) -> Value {
        let a2 = self.widen(a, b);
        let b2 = self.widen(b, a2);
        self.op(Op::Binary(op), vec![a2, b2])
    }

    fn add(&mut self, a: Value, b: Value) -> Value {
        self.bin(BinaryOp::FAdd, a, b)
    }

    fn sub(&mut self, a: Value, b: Value) -> Value {
        self.bin(BinaryOp::FSub, a, b)
    }

    fn mul(&mut self, a: Value, b: Value) -> Value {
        self.bin(BinaryOp::FMul, a, b)
    }

    fn div(&mut self, a: Value, b: Value) -> Value {
        self.bin(BinaryOp::FDiv, a, b)
    }

    fn neg(&mut self, a: Value) -> Value {
        self.op(Op::Unary(UnaryOp::FNeg), vec![a])
    }

    fn math(&mut self, op: MathOp, args: Vec<Value>) -> Value {
        self.op(Op::Math(op), args)
    }

    fn sum(&mut self, a: Option<Value>, b: Option<Value>) -> Option<Value> {
        match (a, b) {
            (Some(a), Some(b)) => Some(self.add(a, b)),
            (a, None) => a,
            (None, b) => b,
        }
    }

    fn diff(&mut self, a: Option<Value>, b: Option<Value>) -> Option<Value> {
        match (a, b) {
            (Some(a), Some(b)) => Some(self.sub(a, b)),
            (a, None) => a,
            (None, Some(b)) => Some(self.neg(b)),
        }
    }

    /// `t * k`.
    fn scale(&mut self, t: Option<Value>, k: Value) -> Option<Value> {
        t.map(|t| self.mul(t, k))
    }
}

/// The constant `x` in every component of a float scalar, vector or matrix type.
fn float_const(m: &mut Module, t: Type, x: f64) -> marlin_shaderc_ir::Const {
    match m.data(t).clone() {
        TypeData::Vector(s, n) => {
            let st = m.scalar(s);
            let c = m.float(st, x);
            m.constant(t, ConstValue::Composite(vec![c; n as usize]))
        }
        TypeData::Matrix {
            element,
            rows,
            cols,
        } => {
            let col = m.vector(element, rows);
            let c = float_const(m, col, x);
            m.constant(t, ConstValue::Composite(vec![c; cols as usize]))
        }
        _ => m.float(t, x),
    }
}

/// The tangent of `op`'s result `r` from its operands `a` and their tangents `t` (`None` is
/// 0); `None` when it is 0.
fn rule(e: &mut Emit, op: &Op, a: &[Value], r: Value, t: &[Option<Value>]) -> Option<Value> {
    let tan = |i: usize| t.get(i).copied().flatten();
    let rty = e.f.ty(r);
    match op {
        Op::Binary(BinaryOp::FAdd) => e.sum(tan(0), tan(1)),
        Op::Binary(BinaryOp::FSub) => e.diff(tan(0), tan(1)),
        Op::Binary(BinaryOp::FMul) => {
            let x = e.scale(tan(0), a[1]);
            let y = e.scale(tan(1), a[0]);
            e.sum(x, y)
        }
        // (ta - r tb) / b
        Op::Binary(BinaryOp::FDiv) => {
            let rb = e.scale(tan(1), r);
            let n = e.diff(tan(0), rb);
            n.map(|n| e.div(n, a[1]))
        }
        // r = a - b trunc(a / b)
        Op::Binary(BinaryOp::FRem) => {
            let q = match tan(1) {
                Some(_) => {
                    let d = e.div(a[0], a[1]);
                    Some(e.math(MathOp::FTrunc, vec![d]))
                }
                None => None,
            };
            let qb = q.and_then(|q| e.scale(tan(1), q));
            e.diff(tan(0), qb)
        }
        Op::Unary(UnaryOp::FNeg) => tan(0).map(|x| e.neg(x)),
        Op::Select => {
            if tan(1).is_none() && tan(2).is_none() {
                return None;
            }
            let x = e.or_zero(tan(1), a[1]);
            let y = e.or_zero(tan(2), a[2]);
            Some(e.op(Op::Select, vec![a[0], x, y]))
        }
        Op::Convert(_) => tan(0).map(|x| e.op_ty(op.clone(), vec![x], Some(rty))),
        Op::Math(m) => math_rule(e, *m, a, r, &tan),
        Op::Splat => tan(0).map(|x| e.op_ty(Op::Splat, vec![x], Some(rty))),
        Op::Construct => {
            if t.iter().all(Option::is_none) {
                return None;
            }
            let parts: Vec<Value> = (0..a.len()).map(|i| e.or_zero(tan(i), a[i])).collect();
            Some(e.op_ty(Op::Construct, parts, Some(rty)))
        }
        Op::MatMul => {
            let x = tan(0).map(|ta| e.op(Op::MatMul, vec![ta, a[1]]));
            let y = tan(1).map(|tb| e.op(Op::MatMul, vec![a[0], tb]));
            e.sum(x, y)
        }
        Op::DiffSeed => tan(1),
        Op::DerivSeed | Op::AssumeUniform | Op::AssumeNonUniform => tan(0),
        // The same operation on the tangents, with the other operands (indices) as they are.
        _ => {
            if t.iter().all(Option::is_none) {
                return None;
            }
            let mut ops = a.to_vec();
            for (i, ti) in t.iter().enumerate() {
                if let Some(ti) = ti {
                    ops[i] = *ti;
                } else if is_carried(op, i) {
                    ops[i] = e.or_zero(None, a[i]);
                }
            }
            Some(e.op(op.clone(), ops))
        }
    }
}

/// Whether operand `i` of a structural operation (`insert`, `shuffle`, ...) is data that has a
/// tangent, rather than an index.
fn is_carried(op: &Op, i: usize) -> bool {
    match op {
        Op::Insert(_) | Op::InsertField(_) | Op::InsertElem(_) | Op::Shuffle(_) => i < 2,
        Op::InsertDyn => i == 0 || i == 2,
        _ => i == 0,
    }
}

fn math_rule(
    e: &mut Emit,
    op: MathOp,
    a: &[Value],
    r: Value,
    tan: &dyn Fn(usize) -> Option<Value>,
) -> Option<Value> {
    use MathOp::*;
    let ty = e.f.ty(r);
    let one = |e: &mut Emit, like: Value| {
        let t = e.f.ty(like);
        e.konst(t, 1.0)
    };
    match op {
        Fma => {
            let x = e.scale(tan(0), a[1]);
            let y = e.scale(tan(1), a[0]);
            let s = e.sum(x, y);
            e.sum(s, tan(2))
        }
        // t / 2r
        Sqrt => tan(0).map(|x| {
            let two = e.konst(ty, 2.0);
            let d = e.mul(r, two);
            e.div(x, d)
        }),
        // -r³ t / 2
        Rsqrt => tan(0).map(|x| {
            let r2 = e.mul(r, r);
            let r3 = e.mul(r2, r);
            let k = e.konst(ty, -0.5);
            let s = e.mul(r3, k);
            e.mul(x, s)
        }),
        // -r² t
        Rcp => tan(0).map(|x| {
            let r2 = e.mul(r, r);
            let s = e.neg(r2);
            e.mul(x, s)
        }),
        Exp2 => tan(0).map(|x| {
            let k = e.konst(ty, LN_2);
            let s = e.mul(r, k);
            e.mul(x, s)
        }),
        Log2 => tan(0).map(|x| {
            let k = e.konst(ty, LN_2);
            let d = e.mul(a[0], k);
            e.div(x, d)
        }),
        Sin => tan(0).map(|x| {
            let c = e.math(Cos, vec![a[0]]);
            e.mul(x, c)
        }),
        Cos => tan(0).map(|x| {
            let s = e.math(Sin, vec![a[0]]);
            let p = e.mul(x, s);
            e.neg(p)
        }),
        Abs => tan(0).map(|x| {
            let s = e.math(Sign, vec![a[0]]);
            e.mul(x, s)
        }),
        // The tangent of the operand that is the result.
        FMin | FMax => {
            if tan(0).is_none() && tan(1).is_none() {
                return None;
            }
            let c = e.op(Op::Fcmp(FloatCmp::Oeq), vec![a[0], r]);
            let x = e.or_zero(tan(0), a[0]);
            let y = e.or_zero(tan(1), a[1]);
            Some(e.op(Op::Select, vec![c, x, y]))
        }
        Fract => tan(0),
        Dot | Cross => {
            let x = tan(0).map(|ta| e.math(op, vec![ta, a[1]]));
            let y = tan(1).map(|tb| e.math(op, vec![a[0], tb]));
            e.sum(x, y)
        }
        // y x^(y-1) tx + r ln(x) ty
        Pow => {
            let x = tan(0).map(|tx| {
                let o = one(e, a[1]);
                let ym1 = e.sub(a[1], o);
                let p = e.math(Pow, vec![a[0], ym1]);
                let k = e.mul(a[1], p);
                e.mul(tx, k)
            });
            let y = tan(1).map(|ty| {
                let l = e.math(Log, vec![a[0]]);
                let k = e.mul(r, l);
                e.mul(ty, k)
            });
            e.sum(x, y)
        }
        // (1 + r²) t
        Tan => tan(0).map(|x| {
            let o = one(e, r);
            let r2 = e.mul(r, r);
            let s = e.add(o, r2);
            e.mul(x, s)
        }),
        // ± t / sqrt(1 - x²)
        Asin | Acos => tan(0).map(|x| {
            let o = one(e, a[0]);
            let x2 = e.mul(a[0], a[0]);
            let d = e.sub(o, x2);
            let s = e.math(Rsqrt, vec![d]);
            let p = e.mul(x, s);
            if op == Acos { e.neg(p) } else { p }
        }),
        // t / (1 + x²)
        Atan => tan(0).map(|x| {
            let o = one(e, a[0]);
            let x2 = e.mul(a[0], a[0]);
            let d = e.add(o, x2);
            e.div(x, d)
        }),
        // atan2(y, x): (x ty - y tx) / (x² + y²)
        Atan2 => {
            let n1 = e.scale(tan(0), a[1]);
            let n2 = e.scale(tan(1), a[0]);
            let n = e.diff(n1, n2);
            n.map(|n| {
                let x2 = e.mul(a[1], a[1]);
                let y2 = e.mul(a[0], a[0]);
                let d = e.add(x2, y2);
                e.div(n, d)
            })
        }
        Sinh => tan(0).map(|x| {
            let c = e.math(Cosh, vec![a[0]]);
            e.mul(x, c)
        }),
        Cosh => tan(0).map(|x| {
            let s = e.math(Sinh, vec![a[0]]);
            e.mul(x, s)
        }),
        // (1 - r²) t
        Tanh => tan(0).map(|x| {
            let o = one(e, r);
            let r2 = e.mul(r, r);
            let s = e.sub(o, r2);
            e.mul(x, s)
        }),
        Exp => tan(0).map(|x| e.mul(x, r)),
        Log => tan(0).map(|x| e.div(x, a[0])),
        // (t - r (r · t)) / |v|
        Normalize => tan(0).map(|x| {
            let d = e.math(Dot, vec![r, x]);
            let p = e.mul(r, d);
            let n = e.sub(x, p);
            let len = e.math(Length, vec![a[0]]);
            e.div(n, len)
        }),
        // (v · t) / r
        Length => tan(0).map(|x| {
            let d = e.math(Dot, vec![a[0], x]);
            e.div(d, r)
        }),
        Distance => {
            let td = e.diff(tan(0), tan(1));
            td.map(|td| {
                let dv = e.sub(a[0], a[1]);
                let d = e.math(Dot, vec![dv, td]);
                e.div(d, r)
            })
        }
        // r = i - 2 (n · i) n: ti - 2 ((n · i)' n + (n · i) tn)
        Reflect => {
            let (i, n) = (a[0], a[1]);
            let x = tan(1).map(|tn| e.math(Dot, vec![tn, i]));
            let y = tan(0).map(|ti| e.math(Dot, vec![n, ti]));
            let td = e.sum(x, y);
            let p1 = td.map(|td| e.mul(td, n));
            let p2 = tan(1).map(|tn| {
                let d = e.math(Dot, vec![n, i]);
                e.mul(d, tn)
            });
            let inner = e.sum(p1, p2);
            let twice = inner.map(|x| {
                let two = e.konst(ty, 2.0);
                e.mul(x, two)
            });
            e.diff(tan(0), twice)
        }
        Refract => refract(e, a, tan),
        // u = (x - lo) / (hi - lo), c = saturate(u), r = c² (3 - 2c): 6 c (1 - c) u'
        Smoothstep => {
            let (lo, hi, x) = (a[0], a[1], a[2]);
            let tw = e.diff(tan(1), tan(0));
            let txl = e.diff(tan(2), tan(0));
            let w = e.sub(hi, lo);
            let xl = e.sub(x, lo);
            let u = e.div(xl, w);
            let utw = e.scale(tw, u);
            let n = e.diff(txl, utw);
            n.map(|n| {
                let tu = e.div(n, w);
                let z = e.konst(ty, 0.0);
                let o = e.konst(ty, 1.0);
                let lo = e.math(FMax, vec![u, z]);
                let c = e.math(FMin, vec![lo, o]);
                let omc = e.sub(o, c);
                let p = e.mul(c, omc);
                let six = e.konst(ty, 6.0);
                let k = e.mul(p, six);
                e.mul(tu, k)
            })
        }
        // `n` or `-n`, as the result chose.
        FaceForward => tan(0).map(|tn| {
            let d = e.math(Dot, vec![a[2], a[1]]);
            let dt = e.f.ty(d);
            let z = e.konst(dt, 0.0);
            let c = e.op(Op::Fcmp(FloatCmp::Olt), vec![d, z]);
            let nt = e.neg(tn);
            e.op(Op::Select, vec![c, tn, nt])
        }),
        _ => None,
    }
}

/// refract(i, n, eta) = 0 where k < 0, else eta i - (eta d + sqrt(k)) n, with d = n · i and
/// k = 1 - eta² (1 - d²).
fn refract(e: &mut Emit, a: &[Value], tan: &dyn Fn(usize) -> Option<Value>) -> Option<Value> {
    let (i, n, eta) = (a[0], a[1], a[2]);
    let (ti, tn, te) = (tan(0), tan(1), tan(2));
    if ti.is_none() && tn.is_none() && te.is_none() {
        return None;
    }
    let st = e.f.ty(eta);
    let vt = e.f.ty(i);
    let d = e.math(MathOp::Dot, vec![n, i]);
    let one = e.konst(st, 1.0);
    let d2 = e.mul(d, d);
    let omd2 = e.sub(one, d2);
    let e2 = e.mul(eta, eta);
    let ek = e.mul(e2, omd2);
    let k = e.sub(one, ek);
    let s = e.math(MathOp::Sqrt, vec![k]);
    // d' = tn · i + n · ti
    let x = tn.map(|tn| e.math(MathOp::Dot, vec![tn, i]));
    let y = ti.map(|ti| e.math(MathOp::Dot, vec![n, ti]));
    let td = e.sum(x, y);
    // k' = -2 eta eta' (1 - d²) + 2 eta² d d'
    let ka = te.map(|te| {
        let p = e.mul(eta, te);
        let q = e.mul(p, omd2);
        let two = e.konst(st, -2.0);
        e.mul(q, two)
    });
    let kb = td.map(|td| {
        let p = e.mul(e2, d);
        let q = e.mul(p, td);
        let two = e.konst(st, 2.0);
        e.mul(q, two)
    });
    let tk = e.sum(ka, kb);
    // s' = k' / 2s
    let ts = tk.map(|tk| {
        let two = e.konst(st, 2.0);
        let d = e.mul(s, two);
        e.div(tk, d)
    });
    // c' = eta' d + eta d' + s'
    let c1 = te.map(|te| e.mul(te, d));
    let c2 = td.map(|td| e.mul(eta, td));
    let c12 = e.sum(c1, c2);
    let tc = e.sum(c12, ts);
    // r' = eta' i + eta ti - c' n - (eta d + s) tn
    let p1 = te.map(|te| e.mul(te, i));
    let p2 = ti.map(|ti| e.mul(eta, ti));
    let p3 = tc.map(|tc| e.mul(tc, n));
    let p4 = tn.map(|tn| {
        let ed = e.mul(eta, d);
        let c = e.add(ed, s);
        e.mul(c, tn)
    });
    let p12 = e.sum(p1, p2);
    let p123 = e.diff(p12, p3);
    let tr = e.diff(p123, p4);
    tr.map(|tr| {
        let z = e.konst(st, 0.0);
        let c = e.op(Op::Fcmp(FloatCmp::Olt), vec![k, z]);
        let zv = e.zero(vt);
        e.op(Op::Select, vec![c, zv, tr])
    })
}
