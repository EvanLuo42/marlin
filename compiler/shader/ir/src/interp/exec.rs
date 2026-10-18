//! Running functions: structured control flow over lane sets, memory, atomics, waves, calls.

use std::collections::HashMap;

use crate::func::{Block, FuncId, Function, Inst, Value, ValueDef};
use crate::interp::eval::{binary_int, eval, from_f64, mask, sext, to_f64};
use crate::interp::memory::{self, field_offset};
use crate::interp::{ExecError, Interpreter, Output, Print, Ptr, Resources, Root, Val};
use crate::module::{Binding, Builtin, Const, GlobalId, Module, Stage};
use crate::op::{AccessStep, AtomicOp, BinaryOp, Op, QuadOp, ReduceOp, VoteOp, WaveOp};
use crate::print::FuncText;
use crate::ty::{ResourceKind, Scalar, Space, Type, TypeData};

type R<T> = Result<T, ExecError>;

/// Where a lane went when it left a region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Exit {
    Jump(Block),
    Return,
}

struct Frame<'m> {
    id: FuncId,
    f: &'m Function,
    /// `vals[value * lanes + lane]`.
    vals: Vec<Option<Val>>,
    /// Blocks of enclosing regions (merges, loops' continue blocks): a jump there leaves the
    /// region being run.
    exits: Vec<Block>,
    returns: Vec<Option<Vec<Val>>>,
    depth: u32,
}

struct Machine<'m, 'r> {
    m: &'m Module,
    lanes: usize,
    wave: usize,
    group: [u32; 3],
    size: [u32; 3],
    step_limit: u64,
    vars: Vec<Val>,
    globals: HashMap<(GlobalId, u32), Val>,
    consts: HashMap<Const, Val>,
    resources: &'r mut Resources,
    out: &'r mut Output,
    /// Lanes that have returned from the entry point.
    finished: Vec<bool>,
}

/// The call depth past which a run is an error (recursion the verifier would reject).
const MAX_DEPTH: u32 = 256;

pub(super) fn call(
    it: &Interpreter<'_>,
    func: FuncId,
    args: &[Val],
    resources: &mut Resources,
    out: &mut Output,
) -> R<Vec<Val>> {
    let m = it.module;
    let f = m
        .funcs
        .get(func)
        .ok_or_else(|| ExecError::new("no such function"))?;
    if args.len() != f.params().len() {
        return Err(ExecError::new(format!(
            "@{} takes {} argument(s), {} given",
            f.name,
            f.params().len(),
            args.len()
        )));
    }
    let mut machine = Machine::new(it, [0; 3], [1, 1, 1], resources, out)?;
    let lane_args: Vec<Vec<Val>> = vec![args.to_vec()];
    let mut returns = machine.call(func, &[0], &lane_args, 0)?;
    Ok(returns.pop().flatten().unwrap_or_default())
}

pub(super) fn dispatch(
    it: &Interpreter<'_>,
    entry: FuncId,
    groups: [u32; 3],
    resources: &mut Resources,
    out: &mut Output,
) -> R<()> {
    let m = it.module;
    let e = m
        .entry_of(entry)
        .ok_or_else(|| ExecError::new("not an entry point"))?;
    let Stage::Compute(size) = e.stage else {
        return Err(ExecError::new(format!(
            "only compute entry points run; this one is {}",
            e.stage.name()
        )));
    };
    for z in 0..groups[2] {
        for y in 0..groups[1] {
            for x in 0..groups[0] {
                let mut machine = Machine::new(it, [x, y, z], size, resources, out)?;
                machine.entry(entry)?;
            }
        }
    }
    Ok(())
}

impl<'m, 'r> Machine<'m, 'r> {
    fn new(
        it: &Interpreter<'m>,
        group: [u32; 3],
        size: [u32; 3],
        resources: &'r mut Resources,
        out: &'r mut Output,
    ) -> R<Self> {
        let lanes = size.iter().map(|&s| s as usize).product::<usize>();
        if lanes == 0 || lanes > 4096 {
            return Err(ExecError::new("a thread group has 1 to 4096 lanes"));
        }
        let wave = it.options.wave_size as usize;
        if !wave.is_power_of_two() || wave > 128 {
            return Err(ExecError::new("the wave size is a power of two up to 128"));
        }
        Ok(Machine {
            m: it.module,
            lanes,
            wave,
            group,
            size,
            step_limit: it.options.step_limit,
            vars: Vec::new(),
            globals: HashMap::new(),
            consts: HashMap::new(),
            resources,
            out,
            finished: vec![false; lanes],
        })
    }

    fn local_id(&self, lane: usize) -> [u32; 3] {
        let (sx, sy) = (self.size[0] as usize, self.size[1] as usize);
        [
            (lane % sx) as u32,
            ((lane / sx) % sy) as u32,
            (lane / (sx * sy)) as u32,
        ]
    }

    fn entry(&mut self, entry: FuncId) -> R<()> {
        let all: Vec<usize> = (0..self.lanes).collect();
        let args = vec![Vec::new(); self.lanes];
        self.call(entry, &all, &args, 0)?;
        Ok(())
    }

    /// Runs `func` for `lanes` (with each lane's arguments) and gives each lane's results.
    fn call(
        &mut self,
        id: FuncId,
        lanes: &[usize],
        args: &[Vec<Val>],
        depth: u32,
    ) -> R<Vec<Option<Vec<Val>>>> {
        if depth > MAX_DEPTH {
            return Err(ExecError::new("calls nest too deeply (recursion?)"));
        }
        let f = &self.m.funcs[id];
        let entry = f
            .entry()
            .ok_or_else(|| ExecError::new(format!("@{} has no body", f.name)))?;
        let mut fr = Frame {
            id,
            f,
            vals: vec![None; f.values.len() * self.lanes],
            exits: Vec::new(),
            returns: vec![None; self.lanes],
            depth,
        };
        for (k, &lane) in lanes.iter().enumerate() {
            for (&p, a) in f.params().iter().zip(&args[k]) {
                self.set(&mut fr, p, lane, a.clone());
            }
        }
        let exits = self.region(&mut fr, lanes.to_vec(), entry, None)?;
        for (lane, exit) in exits {
            if exit != Exit::Return {
                let _ = lane;
                return Err(self.error(&fr, None, "a jump leaves the function"));
            }
        }
        Ok(lanes.iter().map(|&l| fr.returns[l].take()).collect())
    }

    fn error(&self, fr: &Frame<'_>, inst: Option<Inst>, message: impl Into<String>) -> ExecError {
        ExecError {
            func: Some(fr.f.name.clone()),
            at: inst.map(|i| FuncText::new(self.m, fr.id).inst(i)),
            message: message.into(),
        }
    }

    fn set(&self, fr: &mut Frame<'_>, v: Value, lane: usize, val: Val) {
        fr.vals[v.index() * self.lanes + lane] = Some(val);
    }

    fn get(&mut self, fr: &Frame<'_>, v: Value, lane: usize) -> R<Val> {
        match fr.f.values[v].def {
            ValueDef::Const(c) => {
                if let Some(val) = self.consts.get(&c) {
                    return Ok(val.clone());
                }
                let val = memory::constant(self.m, c)?;
                self.consts.insert(c, val.clone());
                Ok(val)
            }
            ValueDef::Global(g) => {
                let per_lane = matches!(self.space_of(g), Space::Input | Space::Output);
                let key = if per_lane { lane as u32 } else { 0 };
                Ok(Val::Ptr(Ptr::Logical {
                    root: Root::Global(g, key),
                    path: Vec::new(),
                }))
            }
            _ => fr.vals[v.index() * self.lanes + lane]
                .clone()
                .ok_or_else(|| self.error(fr, None, "a value is read before it is computed")),
        }
    }

    fn space_of(&self, g: GlobalId) -> Space {
        match self.m.data(self.m.globals[g].ty) {
            TypeData::Ptr { space, .. } => *space,
            _ => Space::Function,
        }
    }

    fn step(&mut self, fr: &Frame<'_>, inst: Inst) -> R<()> {
        self.out.steps += 1;
        if self.out.steps > self.step_limit {
            return Err(self.error(
                fr,
                Some(inst),
                "the step limit is reached (a loop that does not end?)",
            ));
        }
        Ok(())
    }

    // ---- control flow ----

    /// Passes each lane's jump arguments to `target`'s parameters.
    fn pass(
        &mut self,
        fr: &mut Frame<'_>,
        target: Block,
        args: &[Value],
        lanes: &[usize],
    ) -> R<()> {
        let params = fr.f.blocks[target].params.clone();
        for &lane in lanes {
            let vals: Vec<Val> = args
                .iter()
                .map(|&a| self.get(fr, a, lane))
                .collect::<R<_>>()?;
            for (&p, v) in params.iter().zip(vals) {
                self.set(fr, p, lane, v);
            }
        }
        Ok(())
    }

    /// Whether going to `b` leaves the region that stops at `stop`.
    fn leaves(fr: &Frame<'_>, b: Block, stop: Option<Block>) -> bool {
        Some(b) == stop || fr.exits.contains(&b)
    }

    /// Runs `lanes` into `target` until they reach `stop`: those land in `arrived`, the rest
    /// (breaks, continues, returns) in `out`.
    fn enter(
        &mut self,
        fr: &mut Frame<'_>,
        lanes: Vec<usize>,
        target: Block,
        stop: Block,
        arrived: &mut Vec<usize>,
        out: &mut Vec<(usize, Exit)>,
    ) -> R<()> {
        if lanes.is_empty() {
            return Ok(());
        }
        if target == stop {
            arrived.extend(lanes);
            return Ok(());
        }
        if fr.exits.contains(&target) {
            out.extend(lanes.into_iter().map(|l| (l, Exit::Jump(target))));
            return Ok(());
        }
        for (lane, exit) in self.region(fr, lanes, target, Some(stop))? {
            match exit {
                Exit::Jump(b) if b == stop => arrived.push(lane),
                other => out.push((lane, other)),
            }
        }
        Ok(())
    }

    /// Runs `lanes` from `start` (which runs) until they jump to `stop` or out of the region.
    fn region(
        &mut self,
        fr: &mut Frame<'_>,
        mut lanes: Vec<usize>,
        mut block: Block,
        stop: Option<Block>,
    ) -> R<Vec<(usize, Exit)>> {
        let mut out = Vec::new();
        loop {
            if lanes.is_empty() {
                return Ok(out);
            }
            let insts: Vec<Inst> = fr.f.insts(block).collect();
            let Some((&last, body)) = insts.split_last() else {
                return Err(self.error(fr, None, "a block without instructions"));
            };
            for &inst in body {
                self.step(fr, inst)?;
                self.inst(fr, inst, &lanes)?;
            }
            self.step(fr, last)?;
            let op = fr.f.insts[last].op.clone();
            let ops: Vec<Value> = fr.f.operands(last).to_vec();
            let mut next = match &op {
                Op::Br(t) => {
                    self.pass(fr, *t, &ops, &lanes)?;
                    *t
                }
                Op::Return => {
                    for &lane in &lanes {
                        let vals: Vec<Val> = ops
                            .iter()
                            .map(|&v| self.get(fr, v, lane))
                            .collect::<R<_>>()?;
                        fr.returns[lane] = Some(vals);
                        if fr.depth == 0 {
                            self.finished[lane] = true;
                        }
                    }
                    out.extend(lanes.iter().map(|&l| (l, Exit::Return)));
                    return Ok(out);
                }
                Op::Unreachable => {
                    return Err(self.error(fr, Some(last), "`unreachable` is reached"));
                }
                Op::CondBr { merge, .. } | Op::Switch { merge, .. } => {
                    let merge = *merge;
                    // Which target each lane takes.
                    let succs = op.successors(ops.len());
                    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); succs.len()];
                    for &lane in &lanes {
                        let k = self.choose(fr, last, &op, &ops, lane)?;
                        groups[k].push(lane);
                    }
                    let mut arrived = Vec::new();
                    fr.exits.push(merge);
                    for ((target, range), group) in succs.into_iter().zip(groups) {
                        self.pass(fr, target, &ops[range], &group)?;
                        self.enter(fr, group, target, merge, &mut arrived, &mut out)?;
                    }
                    fr.exits.pop();
                    arrived.sort_unstable();
                    lanes = arrived;
                    merge
                }
                Op::Loop {
                    header,
                    continuing,
                    merge,
                } => {
                    let (header, cont, merge) = (*header, *continuing, *merge);
                    self.pass(fr, header, &ops, &lanes)?;
                    fr.exits.push(cont);
                    fr.exits.push(merge);
                    let mut active = std::mem::take(&mut lanes);
                    let mut broken = Vec::new();
                    while !active.is_empty() {
                        // The body, from the header to the continue block.
                        let mut at_cont = Vec::new();
                        for (lane, exit) in self.region(fr, active, header, Some(cont))? {
                            match exit {
                                Exit::Jump(b) if b == cont => at_cont.push(lane),
                                Exit::Jump(b) if b == merge => broken.push(lane),
                                other => out.push((lane, other)),
                            }
                        }
                        at_cont.sort_unstable();
                        if cont == header {
                            active = at_cont;
                            continue;
                        }
                        // The continue block, back to the header.
                        active = Vec::new();
                        if at_cont.is_empty() {
                            break;
                        }
                        for (lane, exit) in self.region(fr, at_cont, cont, Some(header))? {
                            match exit {
                                Exit::Jump(b) if b == header => active.push(lane),
                                Exit::Jump(b) if b == merge => broken.push(lane),
                                other => out.push((lane, other)),
                            }
                        }
                        active.sort_unstable();
                    }
                    fr.exits.truncate(fr.exits.len() - 2);
                    broken.sort_unstable();
                    lanes = broken;
                    merge
                }
                _ => {
                    return Err(self.error(fr, Some(last), "a block does not end in a terminator"));
                }
            };
            if Self::leaves(fr, next, stop) {
                out.extend(lanes.drain(..).map(|l| (l, Exit::Jump(next))));
                return Ok(out);
            }
            std::mem::swap(&mut block, &mut next);
        }
    }

    /// The index (into the terminator's successors) of the target `lane` takes.
    fn choose(
        &mut self,
        fr: &Frame<'_>,
        inst: Inst,
        op: &Op,
        ops: &[Value],
        lane: usize,
    ) -> R<usize> {
        let v = self.get(fr, ops[0], lane)?;
        let bits = v.bits().ok_or_else(|| {
            self.error(fr, Some(inst), "a branch on something that is not a scalar")
        })?;
        Ok(match op {
            Op::CondBr { .. } => usize::from(bits & 1 == 0),
            Op::Switch { cases, .. } => {
                let s = self
                    .m
                    .element_scalar(fr.f.ty(ops[0]))
                    .unwrap_or(Scalar::I32);
                let b = bits & mask(s);
                cases
                    .iter()
                    .position(|c| c.value == b)
                    .unwrap_or(cases.len())
            }
            _ => 0,
        })
    }

    // ---- instructions ----

    fn inst(&mut self, fr: &mut Frame<'_>, inst: Inst, lanes: &[usize]) -> R<()> {
        let f = fr.f;
        let data = &f.insts[inst];
        let ops: Vec<Value> = f.operands(inst).to_vec();
        let tys: Vec<Type> = ops.iter().map(|&v| f.ty(v)).collect();
        let result_ty = data.results.first().map(|&r| f.ty(r));
        let op = &data.op;
        let (fname, fid) = (fr.f.name.clone(), fr.id);
        let wrap = |e: ExecError, s: &Self| {
            if e.func.is_some() {
                e
            } else {
                ExecError {
                    func: Some(fname.clone()),
                    at: Some(FuncText::new(s.m, fid).inst(inst)),
                    message: e.message,
                }
            }
        };
        match op {
            Op::Wave { op, cluster } => {
                return self
                    .wave(fr, inst, *op, *cluster, &ops, lanes)
                    .map_err(|e| wrap(e, self));
            }
            Op::Call(callee) => {
                let mut args = Vec::with_capacity(lanes.len());
                for &lane in lanes {
                    args.push(
                        ops.iter()
                            .map(|&v| self.get(fr, v, lane))
                            .collect::<R<Vec<_>>>()?,
                    );
                }
                let results = self.call(*callee, lanes, &args, fr.depth + 1)?;
                for (&lane, r) in lanes.iter().zip(results) {
                    for (&res, v) in data.results.iter().zip(r.unwrap_or_default()) {
                        self.set(fr, res, lane, v);
                    }
                }
                return Ok(());
            }
            Op::Barrier { .. } => {
                let live = self.finished.iter().filter(|f| !**f).count();
                if fr.depth == 0 && lanes.len() != live {
                    return Err(self.error(
                        fr,
                        Some(inst),
                        "a barrier in control flow that not every lane reaches",
                    ));
                }
                return Ok(());
            }
            Op::Discard | Op::IsHelper | Op::Deriv { .. } => {
                return Err(self.error(fr, Some(inst), "pixel operations are not interpreted yet"));
            }
            Op::TexSample(_)
            | Op::TexLoad(_)
            | Op::TexStore(_)
            | Op::TexAtomic { .. }
            | Op::TexGather { .. }
            | Op::TexQuery(_) => {
                return Err(self.error(fr, Some(inst), "textures are not interpreted yet"));
            }
            Op::RqInit | Op::RqProceed | Op::RqGet { .. } | Op::RqCommitTriangle => {
                return Err(self.error(fr, Some(inst), "ray queries are not interpreted yet"));
            }
            _ => {}
        }
        for &lane in lanes {
            let args: Vec<Val> = ops
                .iter()
                .map(|&v| self.get(fr, v, lane))
                .collect::<R<_>>()?;
            let result = self
                .lane_inst(fr, inst, op, &args, &tys, result_ty, lane)
                .map_err(|e| wrap(e, self))?;
            if let (Some(v), Some(&r)) = (result, data.results.first()) {
                self.set(fr, r, lane, v);
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn lane_inst(
        &mut self,
        fr: &Frame<'_>,
        inst: Inst,
        op: &Op,
        args: &[Val],
        tys: &[Type],
        result_ty: Option<Type>,
        lane: usize,
    ) -> R<Option<Val>> {
        let m = self.m;
        Ok(Some(match op {
            Op::Var => {
                let ty = result_ty.ok_or_else(|| ExecError::new("`var` without a type"))?;
                let TypeData::Ptr { pointee, .. } = m.data(ty) else {
                    return Err(ExecError::new("`var` without a pointer type"));
                };
                let id = self.vars.len() as u32;
                let init = match m.data(*pointee) {
                    TypeData::RayQuery => Val::Bits(0),
                    _ => memory::zero(m, *pointee)?,
                };
                self.vars.push(init);
                Val::Ptr(Ptr::Logical {
                    root: Root::Var(id),
                    path: Vec::new(),
                })
            }
            Op::Load { .. } => {
                let ty = result_ty.ok_or_else(|| ExecError::new("`load` without a type"))?;
                self.load(&args[0], ty, lane)?
            }
            Op::Store { .. } => {
                let ty = tys[1];
                self.store(&args[0], ty, &args[1], lane)?;
                return Ok(None);
            }
            Op::Access(steps) => self.access(&args[0], tys[0], steps, &args[1..])?,
            Op::DescGet { .. } => {
                let i = args[0]
                    .bits()
                    .ok_or_else(|| ExecError::new("a resource index is a scalar"))?;
                Val::Handle(i as u32)
            }
            Op::BufferElem | Op::BufferByte | Op::UniformPtr => {
                let Val::Handle(h) = args[0] else {
                    return Err(ExecError::new("expected a handle"));
                };
                let offset = match op {
                    Op::UniformPtr => 0,
                    Op::BufferByte => args[1].bits().unwrap_or(0) as u32 as u64,
                    _ => {
                        let pointee = match m.data(result_ty.unwrap_or(tys[0])) {
                            TypeData::Ptr { pointee, .. } => *pointee,
                            _ => return Err(ExecError::new("expected a pointer result")),
                        };
                        let l = memory::layout(m, pointee)?;
                        let stride = memory::round_up(l.size, l.align);
                        let i = sext(args[1].bits().unwrap_or(0), Scalar::I32);
                        if i < 0 {
                            u64::MAX
                        } else {
                            (i as u64).saturating_mul(stride)
                        }
                    }
                };
                Val::Ptr(Ptr::Bytes { buffer: h, offset })
            }
            Op::BufferLength => {
                let Val::Handle(h) = args[0] else {
                    return Err(ExecError::new("expected a handle"));
                };
                let len = self
                    .resources
                    .buffers
                    .get(h as usize)
                    .map_or(0, |b| b.len() as u64);
                let n = match m.data(tys[0]) {
                    TypeData::Handle(k)
                        if matches!(
                            k.kind,
                            ResourceKind::Buffer | ResourceKind::ReadOnlyBuffer
                        ) =>
                    {
                        let e = k
                            .element
                            .ok_or_else(|| ExecError::new("a buffer without an element type"))?;
                        let l = memory::layout(m, e)?;
                        len / memory::round_up(l.size, l.align).max(1)
                    }
                    _ => len,
                };
                Val::Bits(n & 0xffff_ffff)
            }
            Op::Atomic { op, .. } => {
                let ty = match m.data(tys[0]) {
                    TypeData::Ptr { pointee, .. } => *pointee,
                    _ => return Err(ExecError::new("an atomic needs a pointer")),
                };
                return self.atomic(*op, &args[0], ty, &args[1..], lane);
            }
            Op::DebugPrint(fmt) | Op::DebugAssert(fmt) => {
                let (cond, values, value_tys) = match op {
                    Op::DebugAssert(_) => {
                        (args[0].bits().unwrap_or(0) & 1 == 1, &args[1..], &tys[1..])
                    }
                    _ => (false, args, tys),
                };
                let text = self.format(*fmt, values, value_tys);
                let print = Print {
                    group: self.group,
                    lane: lane as u32,
                    text,
                };
                match op {
                    Op::DebugAssert(_) if !cond => self.out.failed_asserts.push(print),
                    Op::DebugAssert(_) => {}
                    _ => self.out.prints.push(print),
                }
                return Ok(None);
            }
            _ => {
                let _ = (fr, inst);
                eval(m, op, args, tys, result_ty)?
            }
        }))
    }

    fn format(&self, fmt: u32, values: &[Val], tys: &[Type]) -> String {
        let Some(format) = self.m.formats.get(fmt as usize) else {
            return String::new();
        };
        // Each `{...}` takes the next value; `{{` and `}}` are braces.
        let mut out = String::new();
        let mut next = 0;
        let mut chars = format.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    out.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    out.push('}');
                }
                '{' => {
                    // The hole's kind (`{u}`, `{i}`, `{f}`, `{b}`): only `u` changes the text.
                    let mut kind = String::new();
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                        kind.push(c);
                    }
                    match (values.get(next), tys.get(next)) {
                        (Some(v), Some(&t)) => out.push_str(&v.show_as(self.m, t, kind == "u")),
                        _ => out.push('?'),
                    }
                    next += 1;
                }
                c => out.push(c),
            }
        }
        out
    }

    // ---- memory ----

    fn root(&mut self, root: Root, lane: usize) -> R<&mut Val> {
        let _ = lane;
        match root {
            Root::Var(id) => self
                .vars
                .get_mut(id as usize)
                .ok_or_else(|| ExecError::new("no such variable")),
            Root::Global(g, key) => {
                if !self.globals.contains_key(&(g, key)) {
                    let init = self.global_init(g, key)?;
                    self.globals.insert((g, key), init);
                }
                Ok(self.globals.get_mut(&(g, key)).expect("inserted above"))
            }
        }
    }

    /// The initial value of a module variable: builtins, push data, constant data, or zero.
    fn global_init(&self, g: GlobalId, lane: u32) -> R<Val> {
        let m = self.m;
        let global = &m.globals[g];
        let TypeData::Ptr { space, pointee, .. } = m.data(global.ty) else {
            return Err(ExecError::new("a global's type is a pointer"));
        };
        if let Some(c) = global.init {
            return memory::constant(m, c);
        }
        if *space == Space::Push
            && let Some(push) = &self.resources.push
        {
            return Ok(push.clone());
        }
        if let Binding::Builtin(b) = global.binding {
            let local = self.local_id(lane as usize);
            let v3 = |v: [u32; 3]| Val::Agg(v.iter().map(|&x| Val::u32(x)).collect());
            return Ok(match b {
                Builtin::GroupThreadId => v3(local),
                Builtin::GroupId => v3(self.group),
                Builtin::DispatchThreadId => {
                    v3([0, 1, 2].map(|i| self.group[i] * self.size[i] + local[i]))
                }
                Builtin::GroupIndex => Val::u32(lane),
                _ => {
                    return Err(ExecError::new(format!(
                        "`builtin({})` is not interpreted yet",
                        b.name()
                    )));
                }
            });
        }
        memory::zero(m, *pointee)
    }

    fn load(&mut self, ptr: &Val, ty: Type, lane: usize) -> R<Val> {
        let m = self.m;
        match ptr {
            Val::Ptr(Ptr::Logical { root, path }) => {
                let path = path.clone();
                let root = *root;
                let value = memory::get_path(self.root(root, lane)?, &path).cloned();
                match value {
                    Some(v) => Ok(v),
                    None => {
                        self.out.out_of_bounds += 1;
                        memory::zero(m, ty)
                    }
                }
            }
            Val::Ptr(Ptr::Bytes { buffer, offset }) => {
                let bytes = self
                    .resources
                    .buffers
                    .get(*buffer as usize)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                match memory::read(m, bytes, *offset, ty)? {
                    Some(v) => Ok(v),
                    None => {
                        self.out.out_of_bounds += 1;
                        memory::zero(m, ty)
                    }
                }
            }
            _ => Err(ExecError::new(
                "a load from something that is not a pointer",
            )),
        }
    }

    fn store(&mut self, ptr: &Val, ty: Type, value: &Val, lane: usize) -> R<()> {
        let m = self.m;
        match ptr {
            Val::Ptr(Ptr::Logical { root, path }) => {
                let (root, path) = (*root, path.clone());
                match memory::get_path_mut(self.root(root, lane)?, &path) {
                    Some(slot) => *slot = value.clone(),
                    None => self.out.out_of_bounds += 1,
                }
                Ok(())
            }
            Val::Ptr(Ptr::Bytes { buffer, offset }) => {
                let written = match self.resources.buffers.get_mut(*buffer as usize) {
                    Some(bytes) => memory::write(m, bytes, *offset, ty, value)?,
                    None => false,
                };
                if !written {
                    self.out.out_of_bounds += 1;
                }
                Ok(())
            }
            _ => Err(ExecError::new("a store to something that is not a pointer")),
        }
    }

    fn access(
        &mut self,
        base: &Val,
        base_ty: Type,
        steps: &[AccessStep],
        indices: &[Val],
    ) -> R<Val> {
        let m = self.m;
        let mut ty = match m.data(base_ty) {
            TypeData::Ptr { pointee, .. } => *pointee,
            _ => return Err(ExecError::new("`access` needs a pointer")),
        };
        let mut next = indices.iter();
        let index = |v: Option<&Val>| -> u64 {
            let i = sext(v.and_then(Val::bits).unwrap_or(0), Scalar::I32);
            if i < 0 { u64::MAX } else { i as u64 }
        };
        match base {
            Val::Ptr(Ptr::Logical { root, path }) => {
                let mut path = path.clone();
                for step in steps {
                    match step {
                        AccessStep::Field(i) => path.push(*i as u64),
                        AccessStep::Index => path.push(index(next.next())),
                    }
                    ty = crate::infer::step_type_in(&mut crate::infer::Existing(m), ty, *step)
                        .map_err(ExecError::new)?;
                }
                Ok(Val::Ptr(Ptr::Logical { root: *root, path }))
            }
            Val::Ptr(Ptr::Bytes { buffer, offset }) => {
                let mut offset = *offset;
                for step in steps {
                    let (delta, next_ty) = match step {
                        AccessStep::Field(i) => field_offset(m, ty, *i)?,
                        AccessStep::Index => {
                            let (stride, e) = memory::element(m, ty)?;
                            (index(next.next()).saturating_mul(stride), e)
                        }
                    };
                    offset = offset.saturating_add(delta);
                    ty = next_ty;
                }
                Ok(Val::Ptr(Ptr::Bytes {
                    buffer: *buffer,
                    offset,
                }))
            }
            _ => Err(ExecError::new("`access` needs a pointer")),
        }
    }

    fn atomic(
        &mut self,
        op: AtomicOp,
        ptr: &Val,
        ty: Type,
        values: &[Val],
        lane: usize,
    ) -> R<Option<Val>> {
        let old = self.load(ptr, ty, lane)?;
        let s = self.m.element_scalar(ty).unwrap_or(Scalar::I32);
        let o = old.bits().unwrap_or(0);
        let v = values.first().and_then(Val::bits).unwrap_or(0);
        let float = s.is_float();
        let new = match op {
            AtomicOp::Load => return Ok(Some(old)),
            AtomicOp::Store | AtomicOp::Xchg => Some(v),
            AtomicOp::Add if float => Some(from_f64(to_f64(o, s) + to_f64(v, s), s)),
            AtomicOp::Sub if float => Some(from_f64(to_f64(o, s) - to_f64(v, s), s)),
            AtomicOp::Add => Some(binary_int(BinaryOp::Add, s, o, v)),
            AtomicOp::Sub => Some(binary_int(BinaryOp::Sub, s, o, v)),
            AtomicOp::And => Some(o & v),
            AtomicOp::Or => Some(o | v),
            AtomicOp::Xor => Some(o ^ v),
            AtomicOp::SMin => Some(if sext(v, s) < sext(o, s) { v } else { o }),
            AtomicOp::SMax => Some(if sext(v, s) > sext(o, s) { v } else { o }),
            AtomicOp::UMin => Some(o.min(v)),
            AtomicOp::UMax => Some(o.max(v)),
            AtomicOp::CmpXchg => {
                let new = values.get(1).and_then(Val::bits).unwrap_or(0);
                (o == v).then_some(new)
            }
        };
        if let Some(n) = new {
            self.store(ptr, ty, &Val::Bits(n & mask(s)), lane)?;
        }
        Ok(if op == AtomicOp::Store {
            None
        } else {
            Some(old)
        })
    }

    // ---- waves ----

    fn wave(
        &mut self,
        fr: &mut Frame<'_>,
        inst: Inst,
        op: WaveOp,
        cluster: Option<u32>,
        ops: &[Value],
        lanes: &[usize],
    ) -> R<()> {
        let f = fr.f;
        let result = f.insts[inst].results[0];
        let ty = ops.first().map(|&v| f.ty(v));
        let s = ty
            .and_then(|t| self.m.element_scalar(t))
            .unwrap_or(Scalar::I32);
        // Each lane's operands.
        let mut vals: HashMap<usize, Vec<Val>> = HashMap::new();
        for &lane in lanes {
            vals.insert(
                lane,
                ops.iter()
                    .map(|&v| self.get(fr, v, lane))
                    .collect::<R<_>>()?,
            );
        }
        let wave_of = |lane: usize| lane / self.wave;
        let group_size = cluster
            .map_or(self.wave, |c| (c as usize).min(self.wave))
            .max(1);
        // Active lanes by wave (or cluster), in order.
        let mut sets: HashMap<usize, Vec<usize>> = HashMap::new();
        for &lane in lanes {
            sets.entry(lane / group_size).or_default().push(lane);
        }
        let mut waves: HashMap<usize, Vec<usize>> = HashMap::new();
        for &lane in lanes {
            waves.entry(wave_of(lane)).or_default().push(lane);
        }
        for &lane in lanes {
            let own = &vals[&lane];
            let set = &sets[&(lane / group_size)];
            let wave = &waves[&wave_of(lane)];
            let lane_in_wave = lane % self.wave;
            let value = match op {
                WaveOp::Reduce(r) => {
                    let mut acc: Option<Val> = None;
                    for l in set {
                        let v = &vals[l][0];
                        acc = Some(match acc {
                            None => v.clone(),
                            Some(a) => reduce(r, s, &a, v)?,
                        });
                    }
                    acc.unwrap_or(Val::Bits(0))
                }
                WaveOp::Scan { op: r, exclusive } => {
                    let mut acc: Option<Val> = None;
                    for l in set.iter().take_while(|&&l| l < lane) {
                        let v = &vals[l][0];
                        acc = Some(match acc {
                            None => v.clone(),
                            Some(a) => reduce(r, s, &a, v)?,
                        });
                    }
                    let identity = || identity(r, s, &own[0]);
                    if exclusive {
                        acc.map_or_else(identity, Ok)?
                    } else {
                        match acc {
                            None => own[0].clone(),
                            Some(a) => reduce(r, s, &a, &own[0])?,
                        }
                    }
                }
                WaveOp::Ballot => {
                    let mut words = [0u32; 4];
                    for l in wave {
                        if vals[l][0].bits().unwrap_or(0) & 1 == 1 {
                            let i = l % self.wave;
                            words[i / 32] |= 1 << (i % 32);
                        }
                    }
                    Val::Agg(words.iter().map(|&w| Val::u32(w)).collect())
                }
                WaveOp::Broadcast | WaveOp::Shuffle | WaveOp::Quad(_) => {
                    let base = wave_of(lane) * self.wave;
                    let source = match op {
                        WaveOp::Quad(q) => {
                            let quad = lane & !3;
                            match q {
                                QuadOp::SwapX => lane ^ 1,
                                QuadOp::SwapY => lane ^ 2,
                                QuadOp::SwapDiag => lane ^ 3,
                                QuadOp::ReadLane => {
                                    quad + (own[1].bits().unwrap_or(0) as usize & 3)
                                }
                            }
                        }
                        _ => base + (own[1].bits().unwrap_or(0) as usize % self.wave),
                    };
                    match vals.get(&source) {
                        Some(v) => v[0].clone(),
                        None => memory::zero(self.m, f.ty(result))?,
                    }
                }
                WaveOp::Vote(v) => {
                    let all: Vec<&Val> = wave.iter().map(|l| &vals[l][0]).collect();
                    let truth = |x: &Val| x.bits().unwrap_or(0) & 1 == 1;
                    Val::bool(match v {
                        VoteOp::Any => all.iter().any(|x| truth(x)),
                        VoteOp::All => all.iter().all(|x| truth(x)),
                        VoteOp::Equal => all.windows(2).all(|w| w[0] == w[1]),
                    })
                }
                WaveOp::Match => {
                    let mut words = [0u32; 4];
                    for l in wave {
                        if vals[l][0] == own[0] {
                            let i = l % self.wave;
                            words[i / 32] |= 1 << (i % 32);
                        }
                    }
                    Val::Agg(words.iter().map(|&w| Val::u32(w)).collect())
                }
                WaveOp::LaneIndex => Val::u32(lane_in_wave as u32),
                WaveOp::LaneCount => Val::u32(self.wave as u32),
                WaveOp::IsFirst => Val::bool(wave.first() == Some(&lane)),
            };
            self.set(fr, result, lane, value);
        }
        Ok(())
    }
}

fn reduce(op: ReduceOp, s: Scalar, a: &Val, b: &Val) -> R<Val> {
    let combine = |x: u64, y: u64| -> u64 {
        let (fx, fy) = (to_f64(x, s), to_f64(y, s));
        match op {
            ReduceOp::Add => binary_int(BinaryOp::Add, s, x, y),
            ReduceOp::Mul => binary_int(BinaryOp::Mul, s, x, y),
            ReduceOp::FAdd => from_f64(fx + fy, s),
            ReduceOp::FMul => from_f64(fx * fy, s),
            ReduceOp::SMin => {
                if sext(y, s) < sext(x, s) {
                    y
                } else {
                    x
                }
            }
            ReduceOp::SMax => {
                if sext(y, s) > sext(x, s) {
                    y
                } else {
                    x
                }
            }
            ReduceOp::UMin => x.min(y),
            ReduceOp::UMax => x.max(y),
            ReduceOp::FMin => from_f64(if fy < fx || fx.is_nan() { fy } else { fx }, s),
            ReduceOp::FMax => from_f64(if fy > fx || fx.is_nan() { fy } else { fx }, s),
            ReduceOp::And => x & y,
            ReduceOp::Or => x | y,
            ReduceOp::Xor => x ^ y,
        }
    };
    zip(a, b, &combine)
}

fn zip(a: &Val, b: &Val, f: &impl Fn(u64, u64) -> u64) -> R<Val> {
    Ok(match (a, b) {
        (Val::Bits(x), Val::Bits(y)) => Val::Bits(f(*x, *y)),
        (Val::Agg(xs), Val::Agg(ys)) if xs.len() == ys.len() => Val::Agg(
            xs.iter()
                .zip(ys)
                .map(|(x, y)| zip(x, y, f))
                .collect::<R<_>>()?,
        ),
        _ => return Err(ExecError::new("wave operands of different shapes")),
    })
}

/// The value an exclusive scan gives the first lane.
fn identity(op: ReduceOp, s: Scalar, like: &Val) -> R<Val> {
    let m = mask(s);
    let bits = match op {
        ReduceOp::Add | ReduceOp::Or | ReduceOp::Xor | ReduceOp::UMax => 0,
        ReduceOp::FAdd => from_f64(0.0, s),
        ReduceOp::Mul => 1,
        ReduceOp::FMul => from_f64(1.0, s),
        ReduceOp::And | ReduceOp::UMin => m,
        ReduceOp::SMin => m >> 1,
        ReduceOp::SMax => (m >> 1) + 1,
        ReduceOp::FMin => from_f64(f64::INFINITY, s),
        ReduceOp::FMax => from_f64(f64::NEG_INFINITY, s),
    };
    fn fill(v: &Val, bits: u64) -> Val {
        match v {
            Val::Agg(items) => Val::Agg(items.iter().map(|i| fill(i, bits)).collect()),
            _ => Val::Bits(bits),
        }
    }
    Ok(fill(like, bits))
}
