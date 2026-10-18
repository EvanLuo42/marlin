//! A function's code: blocks in an order where each comes after the blocks that dominate it,
//! block parameters as `OpPhi`, and the IR's structured terminators as SPIR-V's merge
//! instructions (0006 §5.1).
//!
//! A loop's `OpLoopMerge` goes in its header. When the header ends in a selection whose merge
//! is not one of its own targets (more than an `if (...) break;`), the header is split, since a
//! SPIR-V block holds one merge instruction. Two jumps of one `cond_br` to the same block go
//! through a block of their own, since `OpPhi` names each predecessor once.

use std::collections::{HashMap, HashSet};

use marlin_shaderc_ir::{
    AccessStep, AtomicOp, BinaryOp, Block, Builtin, ConvertOp, FloatCmp, Function, Inst, IntCmp,
    MathOp, MemOrder, Op, QuadOp, ReduceOp, Scalar, Scope, Space, Stage, TexArg, Type, TypeData,
    UnaryOp, Value, ValueDef, VoteOp, WaveOp,
};

use crate::spirv::{builtin, capability as cap, glsl, group_operation, op, scope, semantics};
use crate::write::{Global, R, Writer, inst, laid_out, storage_class, unsupported};

pub(crate) struct Body<'w, 'm> {
    w: &'w mut Writer<'m>,
    f: &'m Function,
    code: Vec<u32>,
    ids: HashMap<Value, u32>,
    labels: HashMap<Block, u32>,
    /// The label a block's jumps leave from (its own, or the block split off its header).
    leaves_from: HashMap<Block, u32>,
    /// Loop headers: merge and continue blocks.
    loops: HashMap<Block, (Block, Block)>,
    /// Continue blocks nothing reaches (a loop that never goes round): they still jump back
    /// to their header, since SPIR-V wants every loop to have its back edge.
    stub_continues: HashMap<Block, Block>,
    /// Jumps (block, position among its jumps) that go through a block of their own: the else
    /// jump of a `cond_br` whose two jumps go to one block (`OpPhi` names a predecessor once),
    /// and switch targets outside the switch (a case may only break or continue from a block
    /// inside it). The label of that block.
    via: HashMap<(Block, usize), u32>,
    /// `OpEmitMeshTasksEXT` was just written: it ends the block, so its `return` is not.
    dispatched: bool,
}

impl<'w, 'm> Body<'w, 'm> {
    pub fn new(w: &'w mut Writer<'m>, f: &'m Function) -> Self {
        Body {
            w,
            f,
            code: Vec::new(),
            ids: HashMap::new(),
            labels: HashMap::new(),
            leaves_from: HashMap::new(),
            loops: HashMap::new(),
            stub_continues: HashMap::new(),
            via: HashMap::new(),
            dispatched: false,
        }
    }

    fn emit(&mut self, opcode: u32, operands: &[u32]) {
        self.code.extend(inst(opcode, operands));
    }

    fn ty(&mut self, v: Value) -> R<u32> {
        let t = self.f.ty(v);
        self.w.ty(t, false)
    }

    /// The id of value `v`.
    fn val(&mut self, v: Value) -> R<u32> {
        if let Some(&id) = self.ids.get(&v) {
            return Ok(id);
        }
        let id = match self.f.values[v].def {
            ValueDef::Const(c) => self.w.konst(c)?,
            ValueDef::Global(g) => match self.w.module_var(g)? {
                Global::Var(id) => id,
                Global::Push(_) => {
                    return unsupported("push constants outside the entry block's reach");
                }
            },
            _ => {
                let id = self.w.id();
                self.ids.insert(v, id);
                return Ok(id);
            }
        };
        self.ids.insert(v, id);
        Ok(id)
    }

    /// The blocks in reverse postorder from the entry, then the ones it does not reach.
    fn order(&self) -> Vec<Block> {
        let f = self.f;
        let mut seen = HashSet::new();
        let mut post = Vec::new();
        let entry = f.layout[0];
        let mut stack = vec![(entry, 0usize)];
        seen.insert(entry);
        while let Some((b, i)) = stack.pop() {
            let succs: Vec<Block> = f
                .terminator(b)
                .map(|t| {
                    f.insts[t]
                        .op
                        .successors(f.operands(t).len())
                        .into_iter()
                        .map(|s| s.0)
                        .collect()
                })
                .unwrap_or_default();
            if let Some(&s) = succs.get(i) {
                stack.push((b, i + 1));
                if seen.insert(s) {
                    stack.push((s, 0));
                }
            } else {
                post.push(b);
            }
        }
        post.reverse();
        post.extend(f.layout.iter().filter(|b| !seen.contains(b)));
        post
    }

    /// Whether a loop header's terminator needs a block of its own for its selection merge.
    fn split_header(&self, h: Block) -> bool {
        let Some(t) = self.f.terminator(h) else {
            return false;
        };
        match &self.f.insts[t].op {
            // `if (...) break;` (or `continue`): the loop's merge instruction covers it.
            Op::CondBr {
                then_block,
                else_block,
                merge,
                ..
            } => {
                let (loop_merge, cont) = self.loops[&h];
                let leaves = |b: &Block| *b == loop_merge || *b == cont;
                !((merge == then_block && leaves(else_block))
                    || (merge == else_block && leaves(then_block)))
            }
            Op::Switch { .. } => true,
            _ => false,
        }
    }

    /// The function: `OpFunction` to `OpFunctionEnd`.
    pub fn function(mut self, id: u32) -> R<Vec<u32>> {
        let f = self.f;
        if !f.params().is_empty() || !f.results.is_empty() {
            return unsupported("an entry point with parameters or results");
        }
        for &b in &f.layout {
            let label = self.w.id();
            self.labels.insert(b, label);
            if let Some(t) = f.terminator(b)
                && let Op::Loop {
                    header,
                    continuing,
                    merge,
                } = f.insts[t].op
            {
                self.loops.insert(header, (merge, continuing));
            }
        }
        for (&h, &(_, c)) in &self.loops {
            if f.terminator(c)
                .is_some_and(|t| f.insts[t].op == Op::Unreachable)
            {
                self.stub_continues.insert(c, h);
            }
        }
        let cfg = marlin_shaderc_ir::cfg::Cfg::new(f);
        // SPIR-V judges constructs by structural dominance (with merge and continue edges).
        let sdom = marlin_shaderc_ir::cfg::Structure::new(f, &cfg).sdom;
        for &b in &f.layout {
            let mut from = self.labels[&b];
            if self.loops.contains_key(&b) && self.split_header(b) {
                from = self.w.id();
            }
            self.leaves_from.insert(b, from);
            let Some(t) = f.terminator(b) else { continue };
            match &f.insts[t].op {
                Op::CondBr {
                    then_block,
                    else_block,
                    ..
                } if then_block == else_block => {
                    let via = self.w.id();
                    self.via.insert((b, 1), via);
                }
                Op::Switch { merge, .. } => {
                    let jumps = f.insts[t].op.successors(f.operands(t).len());
                    let bi = cfg.index[&b];
                    for (k, (target, _)) in jumps.into_iter().enumerate() {
                        let ti = cfg.index[&target];
                        if target != *merge && !marlin_shaderc_ir::cfg::dominates(&sdom, bi, ti) {
                            let via = self.w.id();
                            self.via.insert((b, k), via);
                        }
                    }
                }
                _ => {}
            }
        }
        let void = self.w.void();
        let fn_ty = self.w.fn_ty(void, &[]);
        self.emit(op::Function, &[void, id, 0, fn_ty]);

        let order = self.order();
        for (k, &b) in order.iter().enumerate() {
            self.block(b, k == 0)?;
        }
        self.emit(op::FunctionEnd, &[]);
        Ok(self.code)
    }

    /// The `OpPhi` operands for block `b`'s parameters: each jump's argument, with the label
    /// it leaves from.
    fn phis(&mut self, b: Block) -> R<()> {
        let f = self.f;
        let params = f.blocks[b].params.clone();
        if params.is_empty() {
            return Ok(());
        }
        // Each parameter's (value id, label it comes from).
        let mut incoming: Vec<Vec<(u32, u32)>> = vec![Vec::new(); params.len()];
        for &p in &f.layout {
            let Some(t) = f.terminator(p) else { continue };
            let jumps = f.insts[t].op.successors(f.operands(t).len());
            for (k, (target, range)) in jumps.into_iter().enumerate() {
                if target != b {
                    continue;
                }
                let from = match self.via.get(&(p, k)) {
                    Some(&v) => v,
                    None => self.leaves_from[&p],
                };
                for (i, &a) in f.operands(t)[range].iter().enumerate() {
                    let v = self.val(a)?;
                    incoming[i].push((v, from));
                }
            }
        }
        // The back edge of a continue block nothing reaches passes nothing in particular.
        let stub = self
            .stub_continues
            .iter()
            .find(|(_, h)| **h == b)
            .map(|(c, _)| *c);
        if let Some(c) = stub {
            let label = self.labels[&c];
            for (i, &p) in params.iter().enumerate() {
                let t = self.ty(p)?;
                let undef = self.w.undef(t);
                incoming[i].push((undef, label));
            }
        }
        for (i, &p) in params.iter().enumerate() {
            let t = self.ty(p)?;
            let id = self.val(p)?;
            let mut ops = vec![t, id];
            for &(v, from) in &incoming[i] {
                ops.push(v);
                ops.push(from);
            }
            self.emit(op::Phi, &ops);
        }
        Ok(())
    }

    fn block(&mut self, b: Block, entry: bool) -> R<()> {
        let f = self.f;
        let label = self.labels[&b];
        self.emit(op::Label, &[label]);
        self.phis(b)?;
        let insts: Vec<Inst> = f.insts(b).collect();
        if entry {
            // Function variables come first in the first block.
            for &i in &insts {
                if f.insts[i].op == Op::Var {
                    let r = f.insts[i].results[0];
                    let t = self.ty(r)?;
                    let id = self.val(r)?;
                    self.emit(
                        op::Variable,
                        &[t, id, crate::spirv::storage_class::Function],
                    );
                }
            }
            self.push_pointers()?;
        }
        for &i in &insts {
            let data = &f.insts[i];
            if data.op == Op::Var {
                continue;
            }
            if data.op.is_terminator() {
                self.terminator(b, i)?;
            } else {
                self.inst(i)?;
            }
        }
        Ok(())
    }

    /// Pointers to the push constants: member 0 of their block, made once at the start.
    fn push_pointers(&mut self) -> R<()> {
        let f = self.f;
        for (v, data) in f.values.iter() {
            let ValueDef::Global(g) = data.def else {
                continue;
            };
            if f.uses(v).is_empty() {
                continue;
            }
            if let Global::Push(var) = self.w.module_var(g)? {
                let t = self.ty(v)?;
                let zero = self.w.uint(0);
                let id = self.w.id();
                self.emit(op::AccessChain, &[t, id, var, zero]);
                self.ids.insert(v, id);
            }
        }
        Ok(())
    }

    fn terminator(&mut self, b: Block, t: Inst) -> R<()> {
        let f = self.f;
        let ops = f.operands(t).to_vec();
        let data = f.insts[t].op.clone();
        if let Some(&(merge, cont)) = self.loops.get(&b) {
            let (m, c) = (self.labels[&merge], self.labels[&cont]);
            self.emit(op::LoopMerge, &[m, c, 0]);
            if self.split_header(b) {
                let from = self.leaves_from[&b];
                self.emit(op::Branch, &[from]);
                self.emit(op::Label, &[from]);
            } else if let Op::CondBr {
                then_block,
                else_block,
                ..
            } = data
            {
                // `if (...) break;` in the header: the loop's merge covers it.
                let c = self.val(ops[0])?;
                let (tl, el) = (self.labels[&then_block], self.labels[&else_block]);
                self.emit(op::BranchConditional, &[c, tl, el]);
                return Ok(());
            }
        }
        match data {
            Op::Br(target) | Op::Loop { header: target, .. } => {
                let l = self.labels[&target];
                self.emit(op::Branch, &[l]);
            }
            Op::CondBr {
                then_block,
                else_block,
                merge,
                ..
            } => {
                let c = self.val(ops[0])?;
                let m = self.labels[&merge];
                self.emit(op::SelectionMerge, &[m, 0]);
                let (tl, el) = (self.target(b, 0, then_block), self.target(b, 1, else_block));
                self.emit(op::BranchConditional, &[c, tl, el]);
                self.through(b);
            }
            Op::Switch {
                cases,
                default,
                merge,
                ..
            } => {
                let s = self.val(ops[0])?;
                let wide = matches!(self.w.m.data(f.ty(ops[0])), TypeData::Scalar(Scalar::I64));
                let m = self.labels[&merge];
                self.emit(op::SelectionMerge, &[m, 0]);
                let mut operands = vec![s, self.target(b, cases.len(), default)];
                for (k, case) in cases.iter().enumerate() {
                    operands.push(case.value as u32);
                    if wide {
                        operands.push((case.value >> 32) as u32);
                    }
                    operands.push(self.target(b, k, case.block));
                }
                self.emit(op::Switch, &operands);
                self.through(b);
            }
            // `OpEmitMeshTasksEXT` ended the block already.
            Op::Return if self.dispatched => self.dispatched = false,
            Op::Return => {
                if ops.is_empty() {
                    self.emit(op::Return, &[]);
                } else {
                    let v = self.val(ops[0])?;
                    self.emit(op::ReturnValue, &[v]);
                }
            }
            Op::Unreachable => match self.stub_continues.get(&b) {
                Some(h) => {
                    let l = self.labels[h];
                    self.emit(op::Branch, &[l]);
                }
                None => self.emit(op::Unreachable, &[]),
            },
            other => return unsupported(format!("the terminator `{other:?}`")),
        }
        Ok(())
    }

    // ---- instructions ----

    /// A result of `ty` computed by `opcode` over `operands`; returns its id.
    fn value(&mut self, ty: u32, opcode: u32, operands: &[u32]) -> u32 {
        let id = self.w.id();
        let mut ops = vec![ty, id];
        ops.extend_from_slice(operands);
        self.emit(opcode, &ops);
        id
    }

    fn ext(&mut self, ty: u32, instruction: u32, operands: &[u32]) -> u32 {
        let set = self.w.glsl();
        let mut ops = vec![set, instruction];
        ops.extend_from_slice(operands);
        self.value(ty, op::ExtInst, &ops)
    }

    fn scalar_of(&self, t: Type) -> Option<Scalar> {
        self.w.m.element_scalar(t)
    }

    /// A constant of type `t` (scalar or vector) with every component `bits`.
    fn splat(&mut self, t: Type, bits: u64) -> R<u32> {
        let s = self
            .scalar_of(t)
            .ok_or_else(|| marlin_shaderc_target::BackendError {
                message: "a constant of a type without a scalar".into(),
            })?;
        let n = self.w.m.width(t).unwrap_or(1);
        Ok(self.w.lit(s, n, bits))
    }

    fn inst(&mut self, i: Inst) -> R<()> {
        let f = self.f;
        let data = &f.insts[i];
        let ops: Vec<Value> = f.operands(i).to_vec();
        let result = data.results.first().copied();
        let rt = match result {
            Some(r) => Some(self.ty(r)?),
            None => None,
        };
        let a = |s: &mut Self, k: usize| s.val(ops[k]);
        let id = match &data.op {
            Op::Binary(b) => {
                let t = f.ty(ops[0]);
                let is_bool = self.scalar_of(t) == Some(Scalar::I1);
                let (x, mut y) = (a(self, 0)?, a(self, 1)?);
                let opcode = match (b, is_bool) {
                    (BinaryOp::And, true) => op::LogicalAnd,
                    (BinaryOp::Or, true) => op::LogicalOr,
                    (BinaryOp::Xor, true) => op::LogicalNotEqual,
                    (BinaryOp::Add, _) => op::IAdd,
                    (BinaryOp::Sub, _) => op::ISub,
                    (BinaryOp::Mul, _) => op::IMul,
                    (BinaryOp::SDiv, _) => op::SDiv,
                    (BinaryOp::UDiv, _) => op::UDiv,
                    (BinaryOp::SRem, _) => op::SRem,
                    (BinaryOp::URem, _) => op::UMod,
                    (BinaryOp::And, _) => op::BitwiseAnd,
                    (BinaryOp::Or, _) => op::BitwiseOr,
                    (BinaryOp::Xor, _) => op::BitwiseXor,
                    (BinaryOp::Shl | BinaryOp::LShr | BinaryOp::AShr, _) => {
                        // The shift count is taken modulo the width (0004 §9.6).
                        let bits = self.scalar_of(t).map_or(32, bits);
                        let mask = self.splat(f.ty(ops[1]), bits - 1)?;
                        let ct = self.ty(ops[1])?;
                        y = self.value(ct, op::BitwiseAnd, &[y, mask]);
                        match b {
                            BinaryOp::Shl => op::ShiftLeftLogical,
                            BinaryOp::LShr => op::ShiftRightLogical,
                            _ => op::ShiftRightArithmetic,
                        }
                    }
                    (BinaryOp::FAdd, _) => op::FAdd,
                    (BinaryOp::FSub, _) => op::FSub,
                    (BinaryOp::FMul, _) => op::FMul,
                    (BinaryOp::FDiv, _) => op::FDiv,
                    (BinaryOp::FRem, _) => op::FRem,
                    (BinaryOp::MulHiS | BinaryOp::MulHiU, _) => {
                        return self.mul_hi(i, *b == BinaryOp::MulHiS, x, y);
                    }
                };
                Some(self.value(rt.unwrap_or(0), opcode, &[x, y]))
            }
            Op::Unary(u) => {
                let t = f.ty(ops[0]);
                let x = a(self, 0)?;
                let opcode = match u {
                    UnaryOp::Neg => op::SNegate,
                    UnaryOp::Not if self.scalar_of(t) == Some(Scalar::I1) => op::LogicalNot,
                    UnaryOp::Not => op::Not,
                    UnaryOp::FNeg => op::FNegate,
                };
                Some(self.value(rt.unwrap_or(0), opcode, &[x]))
            }
            Op::Icmp(c) => {
                let is_bool = self.scalar_of(f.ty(ops[0])) == Some(Scalar::I1);
                let opcode = match (c, is_bool) {
                    (IntCmp::Eq, true) => op::LogicalEqual,
                    (IntCmp::Ne, true) => op::LogicalNotEqual,
                    (IntCmp::Eq, _) => op::IEqual,
                    (IntCmp::Ne, _) => op::INotEqual,
                    (IntCmp::Slt, _) => op::SLessThan,
                    (IntCmp::Sle, _) => op::SLessThanEqual,
                    (IntCmp::Sgt, _) => op::SGreaterThan,
                    (IntCmp::Sge, _) => op::SGreaterThanEqual,
                    (IntCmp::Ult, _) => op::ULessThan,
                    (IntCmp::Ule, _) => op::ULessThanEqual,
                    (IntCmp::Ugt, _) => op::UGreaterThan,
                    (IntCmp::Uge, _) => op::UGreaterThanEqual,
                };
                let (x, y) = (a(self, 0)?, a(self, 1)?);
                Some(self.value(rt.unwrap_or(0), opcode, &[x, y]))
            }
            Op::Fcmp(c) => {
                let opcode = fcmp(*c)?;
                let (x, y) = (a(self, 0)?, a(self, 1)?);
                Some(self.value(rt.unwrap_or(0), opcode, &[x, y]))
            }
            Op::Select => {
                let (c, x, y) = (a(self, 0)?, a(self, 1)?, a(self, 2)?);
                Some(self.value(rt.unwrap_or(0), op::Select, &[c, x, y]))
            }
            Op::Convert(c) => Some(self.convert(*c, ops[0], result.expect("a result"))?),
            Op::Math(m) => Some(self.math(*m, &ops, result.expect("a result"))?),
            Op::Splat => {
                let x = a(self, 0)?;
                let n = self.w.m.width(f.ty(result.expect("a result"))).unwrap_or(1);
                let parts = vec![x; n as usize];
                Some(self.value(rt.unwrap_or(0), op::CompositeConstruct, &parts))
            }
            Op::Extract(k) | Op::ExtractField(k) | Op::ExtractElem(k) | Op::MatColumn(k) => {
                let x = a(self, 0)?;
                Some(self.value(rt.unwrap_or(0), op::CompositeExtract, &[x, *k]))
            }
            Op::MatExtract(r, c) => {
                let x = a(self, 0)?;
                Some(self.value(rt.unwrap_or(0), op::CompositeExtract, &[x, *c, *r]))
            }
            Op::MatRow(r) => {
                let x = a(self, 0)?;
                let cols = match self.w.m.data(f.ty(ops[0])) {
                    TypeData::Matrix { cols, .. } => *cols as u32,
                    _ => 0,
                };
                let et = self
                    .w
                    .scalar(self.scalar_of(f.ty(ops[0])).unwrap_or(Scalar::F32));
                let mut parts = Vec::new();
                for c in 0..cols {
                    parts.push(self.value(et, op::CompositeExtract, &[x, c, *r]));
                }
                Some(self.value(rt.unwrap_or(0), op::CompositeConstruct, &parts))
            }
            Op::Insert(k) | Op::InsertField(k) | Op::InsertElem(k) => {
                let (agg, x) = (a(self, 0)?, a(self, 1)?);
                Some(self.value(rt.unwrap_or(0), op::CompositeInsert, &[x, agg, *k]))
            }
            Op::Shuffle(lanes) => {
                let (x, y) = (a(self, 0)?, a(self, 1)?);
                let mut operands = vec![x, y];
                operands.extend(lanes);
                Some(self.value(rt.unwrap_or(0), op::VectorShuffle, &operands))
            }
            Op::ExtractDyn => {
                let (x, k) = (a(self, 0)?, a(self, 1)?);
                Some(self.value(rt.unwrap_or(0), op::VectorExtractDynamic, &[x, k]))
            }
            Op::InsertDyn => {
                let (x, k, v) = (a(self, 0)?, a(self, 1)?, a(self, 2)?);
                Some(self.value(rt.unwrap_or(0), op::VectorInsertDynamic, &[x, v, k]))
            }
            Op::Construct => {
                let mut parts = Vec::new();
                for k in 0..ops.len() {
                    parts.push(a(self, k)?);
                }
                Some(self.value(rt.unwrap_or(0), op::CompositeConstruct, &parts))
            }
            Op::MatMul => Some(self.matmul(&ops, rt.unwrap_or(0))?),
            Op::Transpose => {
                let x = a(self, 0)?;
                Some(self.value(rt.unwrap_or(0), op::Transpose, &[x]))
            }
            Op::Load { aligned } => Some(self.load(ops[0], result.expect("a result"), *aligned)?),
            Op::Store { aligned } => {
                self.store(ops[0], ops[1], *aligned)?;
                None
            }
            Op::Access(steps) => {
                let base = a(self, 0)?;
                let mut operands = vec![base];
                let mut k = 1;
                for s in steps {
                    match s {
                        AccessStep::Field(n) => operands.push(self.w.uint(*n)),
                        AccessStep::Index => {
                            operands.push(a(self, k)?);
                            k += 1;
                        }
                    }
                }
                Some(self.value(rt.unwrap_or(0), op::AccessChain, &operands))
            }
            Op::DescGet { nonuniform } => {
                let TypeData::Res(kind) = self.w.m.data(f.ty(ops[0])).clone() else {
                    return unsupported("`desc.get` of something that is not a resource");
                };
                let array = self.w.heap_array(&kind)?;
                let sampler = matches!(
                    kind.kind,
                    marlin_shaderc_ir::ResourceKind::Sampler
                        | marlin_shaderc_ir::ResourceKind::ComparisonSampler
                );
                let heap = if sampler {
                    self.w.sampler_heap()
                } else {
                    self.w.heap()
                };
                let index = a(self, 0)?;
                let ptr_t = self
                    .w
                    .untyped_ptr_ty(crate::spirv::storage_class::UniformConstant);
                let p = self.value(ptr_t, op::UntypedAccessChainKHR, &[array, heap, index]);
                // Images and samplers are loaded; a buffer is reached through the pointer.
                let v = if kind.kind.texture().is_some() || sampler {
                    self.value(rt.unwrap_or(0), op::Load, &[p])
                } else {
                    p
                };
                if *nonuniform {
                    self.w.capability(cap::ShaderNonUniform);
                    for id in [index, p, v] {
                        self.w
                            .decorate(id, crate::spirv::decoration::NonUniform, &[]);
                    }
                }
                Some(v)
            }
            Op::TexSample(targs) => Some(self.tex_sample(&ops, targs, rt)?),
            Op::MeshSetCounts => {
                let (nv, np) = (a(self, 0)?, a(self, 1)?);
                self.emit(op::SetMeshOutputsEXT, &[nv, np]);
                None
            }
            Op::MeshDispatch => {
                let (x, y, z) = (a(self, 0)?, a(self, 1)?, a(self, 2)?);
                let mut operands = vec![x, y, z];
                // The module's payload variable, if it has one.
                let payload = self.w.m.globals.iter().find_map(|(g, d)| {
                    matches!(
                        self.w.m.data(d.ty),
                        TypeData::Ptr {
                            space: Space::Payload,
                            ..
                        }
                    )
                    .then_some(g)
                });
                if let Some(g) = payload
                    && let Global::Var(v) = self.w.module_var(g)?
                {
                    operands.push(v);
                }
                self.emit(op::EmitMeshTasksEXT, &operands);
                self.dispatched = true;
                None
            }
            Op::TexLoad(targs) => Some(self.tex_load(&ops, targs, rt)?),
            Op::TexStore(targs) => {
                self.tex_store(&ops, targs)?;
                None
            }
            Op::BufferElem => {
                let (bp, readonly) = self.buffer_pointer(ops[0])?;
                let _ = readonly;
                let zero = self.w.uint(0);
                let index = a(self, 1)?;
                Some(self.value(rt.unwrap_or(0), op::AccessChain, &[bp, zero, index]))
            }
            Op::BufferByte => {
                // A byte buffer is an array of words: the word at `offset / 4`.
                let (bp, _) = self.buffer_pointer(ops[0])?;
                let zero = self.w.uint(0);
                let u32t = self.w.uint_ty(32);
                let two = self.w.uint(2);
                let offset = a(self, 1)?;
                let index = self.value(u32t, op::ShiftRightLogical, &[offset, two]);
                Some(self.value(rt.unwrap_or(0), op::AccessChain, &[bp, zero, index]))
            }
            Op::UniformPtr => {
                let up = self.uniform_pointer(ops[0])?;
                let zero = self.w.uint(0);
                Some(self.value(rt.unwrap_or(0), op::AccessChain, &[up, zero]))
            }
            Op::BufferLength => {
                let (bp, _) = self.buffer_pointer(ops[0])?;
                let u32t = self.w.uint_ty(32);
                let n = self.value(u32t, op::ArrayLength, &[bp, 0]);
                // A byte buffer's length is in bytes.
                Some(if self.is_byte_buffer(ops[0]) {
                    let four = self.w.uint(4);
                    self.value(u32t, op::IMul, &[n, four])
                } else {
                    n
                })
            }
            Op::Atomic {
                op: aop,
                scope: s,
                order,
            } => Some(self.atomic(*aop, *s, *order, &ops, rt)?).flatten(),
            Op::Barrier { exec, mem, spaces } => {
                let mut sem = semantics::AcquireRelease;
                for space in spaces {
                    sem |= match space {
                        Space::GroupShared => semantics::WorkgroupMemory,
                        _ => semantics::UniformMemory,
                    };
                }
                let mem_scope = self.w.uint(scope_of(*mem)?);
                let sem = self.w.uint(sem);
                match exec {
                    Some(e) => {
                        let e = self.w.uint(scope_of(*e)?);
                        self.emit(op::ControlBarrier, &[e, mem_scope, sem]);
                    }
                    None => self.emit(op::MemoryBarrier, &[mem_scope, sem]),
                }
                None
            }
            Op::Wave { op: w, cluster } => Some(self.wave(*w, *cluster, &ops, rt.unwrap_or(0))?),
            Op::AssumeUniform | Op::AssumeNonUniform => {
                // The value itself; with descriptor heaps, access is non-uniform by default.
                let x = a(self, 0)?;
                let r = result.expect("a result");
                match self.ids.entry(r) {
                    std::collections::hash_map::Entry::Occupied(_) => Some(x),
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(x);
                        return Ok(());
                    }
                }
            }
            other => return unsupported(format!("`{other:?}`")),
        };
        if let (Some(r), Some(id)) = (result, id) {
            self.alias(r, id);
        }
        Ok(())
    }

    /// Makes `id` the value of `r` (which may already have an id from a forward use).
    fn alias(&mut self, r: Value, id: u32) {
        if let Some(&pre) = self.ids.get(&r) {
            if pre != id {
                // A use came first (a phi on a back edge): copy into the id it got.
                let t = self.ty(r).unwrap_or(0);
                self.emit(op::CopyObject, &[t, pre, id]);
            }
        } else {
            self.ids.insert(r, id);
        }
    }

    fn mul_hi(&mut self, i: Inst, signed: bool, x: u32, y: u32) -> R<()> {
        let f = self.f;
        let r = f.insts[i].results[0];
        let t = self.ty(r)?;
        let pair = self.w.struct_pair(t);
        let both = self.value(
            pair,
            if signed {
                op::SMulExtended
            } else {
                op::UMulExtended
            },
            &[x, y],
        );
        let hi = self.value(t, op::CompositeExtract, &[both, 1]);
        self.alias(r, hi);
        Ok(())
    }

    fn convert(&mut self, c: ConvertOp, x: Value, r: Value) -> R<u32> {
        let f = self.f;
        let (from, to) = (f.ty(x), f.ty(r));
        let rt = self.ty(r)?;
        let xv = self.val(x)?;
        let fs = self.scalar_of(from);
        let ts = self.scalar_of(to);
        if fs == Some(Scalar::I1) {
            // From a bool: 1 or 0.
            let one = self.splat(
                to,
                if ts.is_some_and(Scalar::is_float) {
                    float_bits(ts.unwrap_or(Scalar::F32), 1.0)
                } else {
                    1
                },
            )?;
            let zero = self.splat(to, 0)?;
            return Ok(self.value(rt, op::Select, &[xv, one, zero]));
        }
        if ts == Some(Scalar::I1) {
            let zero = self.splat(from, 0)?;
            return Ok(self.value(rt, op::INotEqual, &[xv, zero]));
        }
        Ok(match c {
            ConvertOp::SExt => self.value(rt, op::SConvert, &[xv]),
            ConvertOp::ZExt | ConvertOp::Trunc => self.value(rt, op::UConvert, &[xv]),
            ConvertOp::FpExt | ConvertOp::FpTrunc => self.value(rt, op::FConvert, &[xv]),
            ConvertOp::SiToFp => self.value(rt, op::ConvertSToF, &[xv]),
            ConvertOp::UiToFp => self.value(rt, op::ConvertUToF, &[xv]),
            ConvertOp::Bitcast => self.value(rt, op::Bitcast, &[xv]),
            ConvertOp::FpToSi | ConvertOp::FpToUi => {
                // Saturating, NaN to 0, as the interpreter has it: SPIR-V leaves values out
                // of range undefined.
                let signed = c == ConvertOp::FpToSi;
                let fs = fs.unwrap_or(Scalar::F32);
                let w = bits(ts.unwrap_or(Scalar::I32));
                let raw = self.value(
                    rt,
                    if signed {
                        op::ConvertFToS
                    } else {
                        op::ConvertFToU
                    },
                    &[xv],
                );
                let bt = self.w.bool_vec(self.w.m.width(from).unwrap_or(1));
                let (lo_f, hi_f) = if signed {
                    (-(2f64.powi(w as i32 - 1)), 2f64.powi(w as i32 - 1))
                } else {
                    (0.0, 2f64.powi(w as i32))
                };
                let (lo_i, hi_i) = if signed {
                    (1u64 << (w - 1), (1u64 << (w - 1)) - 1)
                } else {
                    (0, if w == 64 { u64::MAX } else { (1u64 << w) - 1 })
                };
                let hi_c = self.splat(from, float_bits(fs, hi_f))?;
                let lo_c = self.splat(from, float_bits(fs, lo_f))?;
                let too_big = self.value(bt, op::FOrdGreaterThanEqual, &[xv, hi_c]);
                let too_small = self.value(bt, op::FOrdLessThan, &[xv, lo_c]);
                let nan = self.value(bt, op::IsNan, &[xv]);
                let hi = self.splat(to, hi_i)?;
                let lo = self.splat(to, lo_i)?;
                let zero = self.splat(to, 0)?;
                let s1 = self.value(rt, op::Select, &[too_small, lo, raw]);
                let s2 = self.value(rt, op::Select, &[too_big, hi, s1]);
                self.value(rt, op::Select, &[nan, zero, s2])
            }
        })
    }

    fn math(&mut self, m: MathOp, ops: &[Value], r: Value) -> R<u32> {
        let f = self.f;
        let rt = self.ty(r)?;
        let mut args = Vec::new();
        for &v in ops {
            args.push(self.val(v)?);
        }
        let t = f.ty(ops[0]);
        let float = self.scalar_of(t).is_some_and(Scalar::is_float);
        let g = |s: &mut Self, n: u32| Ok(s.ext(rt, n, &args));
        match m {
            MathOp::Fma => g(self, glsl::Fma),
            MathOp::Sqrt => g(self, glsl::Sqrt),
            MathOp::Rsqrt => g(self, glsl::InverseSqrt),
            MathOp::Rcp => {
                let one =
                    self.splat(t, float_bits(self.scalar_of(t).unwrap_or(Scalar::F32), 1.0))?;
                Ok(self.value(rt, op::FDiv, &[one, args[0]]))
            }
            MathOp::Exp2 => g(self, glsl::Exp2),
            MathOp::Log2 => g(self, glsl::Log2),
            MathOp::Sin => g(self, glsl::Sin),
            MathOp::Cos => g(self, glsl::Cos),
            MathOp::Tan => g(self, glsl::Tan),
            MathOp::Asin => g(self, glsl::Asin),
            MathOp::Acos => g(self, glsl::Acos),
            MathOp::Atan => g(self, glsl::Atan),
            MathOp::Atan2 => g(self, glsl::Atan2),
            MathOp::Sinh => g(self, glsl::Sinh),
            MathOp::Cosh => g(self, glsl::Cosh),
            MathOp::Tanh => g(self, glsl::Tanh),
            MathOp::Exp => g(self, glsl::Exp),
            MathOp::Log => g(self, glsl::Log),
            MathOp::Pow => g(self, glsl::Pow),
            MathOp::Abs => g(self, if float { glsl::FAbs } else { glsl::SAbs }),
            MathOp::Sign => g(self, if float { glsl::FSign } else { glsl::SSign }),
            MathOp::SMin => g(self, glsl::SMin),
            MathOp::SMax => g(self, glsl::SMax),
            MathOp::UMin => g(self, glsl::UMin),
            MathOp::UMax => g(self, glsl::UMax),
            MathOp::FMin => g(self, glsl::FMin),
            MathOp::FMax => g(self, glsl::FMax),
            MathOp::Floor => g(self, glsl::Floor),
            MathOp::Ceil => g(self, glsl::Ceil),
            MathOp::FTrunc => g(self, glsl::Trunc),
            MathOp::RoundEven => g(self, glsl::RoundEven),
            MathOp::Fract => g(self, glsl::Fract),
            MathOp::Dot if float => Ok(self.value(rt, op::Dot, &args)),
            MathOp::Cross => g(self, glsl::Cross),
            MathOp::Normalize => g(self, glsl::Normalize),
            MathOp::Length => g(self, glsl::Length),
            MathOp::Distance => g(self, glsl::Distance),
            MathOp::Reflect => g(self, glsl::Reflect),
            MathOp::Refract => g(self, glsl::Refract),
            MathOp::FaceForward => g(self, glsl::FaceForward),
            MathOp::Smoothstep => g(self, glsl::SmoothStep),
            MathOp::Step => g(self, glsl::Step),
            MathOp::Popcount => Ok(self.value(rt, op::BitCount, &args)),
            MathOp::BitReverse => Ok(self.value(rt, op::BitReverse, &args)),
            MathOp::FirstBitHiS => g(self, glsl::FindSMsb),
            MathOp::FirstBitHiU => g(self, glsl::FindUMsb),
            MathOp::FirstBitLo => g(self, glsl::FindILsb),
            MathOp::BitfieldExtractS => Ok(self.value(rt, op::BitFieldSExtract, &args)),
            MathOp::BitfieldExtractU => Ok(self.value(rt, op::BitFieldUExtract, &args)),
            MathOp::BitfieldInsert => Ok(self.value(rt, op::BitFieldInsert, &args)),
            MathOp::IsNan => Ok(self.value(rt, op::IsNan, &args)),
            MathOp::IsInf => Ok(self.value(rt, op::IsInf, &args)),
            other => unsupported(format!("`{}`", other.name())),
        }
    }

    fn matmul(&mut self, ops: &[Value], rt: u32) -> R<u32> {
        let f = self.f;
        let (x, y) = (self.val(ops[0])?, self.val(ops[1])?);
        let is_matrix = |s: &Self, v: Value| matches!(s.w.m.data(f.ty(v)), TypeData::Matrix { .. });
        let opcode = match (is_matrix(self, ops[0]), is_matrix(self, ops[1])) {
            (true, true) => op::MatrixTimesMatrix,
            (true, false) => op::MatrixTimesVector,
            (false, true) => op::VectorTimesMatrix,
            (false, false) => return unsupported("`matmul` of two vectors"),
        };
        Ok(self.value(rt, opcode, &[x, y]))
    }

    /// Loads through `p`; aggregates read from laid-out memory become plain values.
    fn load(&mut self, p: Value, r: Value, aligned: Option<u32>) -> R<u32> {
        let f = self.f;
        let ptr = self.val(p)?;
        let (class, pointee) = self.pointer(p)?;
        let plain = self.ty(r)?;
        let laid = self.w.ty(pointee, laid_out(class))?;
        let mut operands = vec![ptr];
        if let Some(n) = aligned {
            operands.extend([0x2, n]);
        }
        let loaded = self.value(laid, op::Load, &operands);
        // `[InstanceId]` starts at 0 on every backend (0002 §3): Vulkan's instance index
        // includes the draw's first instance.
        if let ValueDef::Global(g) = f.values[p].def
            && self.w.m.globals[g].binding
                == marlin_shaderc_ir::Binding::Builtin(Builtin::InstanceId)
        {
            let base_var = self.w.builtin_input(builtin::BaseInstance);
            let u32t = self.w.uint_ty(32);
            let base = self.value(u32t, op::Load, &[base_var]);
            return Ok(self.value(u32t, op::ISub, &[loaded, base]));
        }
        let _ = f;
        Ok(if laid != plain {
            self.value(plain, op::CopyLogical, &[loaded])
        } else {
            loaded
        })
    }

    fn store(&mut self, p: Value, v: Value, aligned: Option<u32>) -> R<()> {
        let ptr = self.val(p)?;
        let (class, pointee) = self.pointer(p)?;
        let mut x = self.val(v)?;
        let laid = self.w.ty(pointee, laid_out(class))?;
        let plain = self.ty(v)?;
        if laid != plain {
            x = self.value(laid, op::CopyLogical, &[x]);
        }
        let mut operands = vec![ptr, x];
        if let Some(n) = aligned {
            operands.extend([0x2, n]);
        }
        self.emit(op::Store, &operands);
        Ok(())
    }

    /// The storage class and pointee of pointer value `p`.
    fn pointer(&self, p: Value) -> R<(u32, Type)> {
        match *self.w.m.data(self.f.ty(p)) {
            TypeData::Ptr { space, pointee, .. } => Ok((storage_class(space)?, pointee)),
            _ => unsupported("a load or store through something that is not a pointer"),
        }
    }

    fn is_byte_buffer(&self, h: Value) -> bool {
        use marlin_shaderc_ir::ResourceKind::{ByteBuffer, ReadOnlyByteBuffer};
        matches!(
            self.w.m.data(self.f.ty(h)),
            TypeData::Handle(k) if matches!(k.kind, ByteBuffer | ReadOnlyByteBuffer)
        )
    }

    /// The pointer to a uniform's block from its handle.
    fn uniform_pointer(&mut self, h: Value) -> R<u32> {
        let TypeData::Handle(kind) = self.w.m.data(self.f.ty(h)).clone() else {
            return unsupported("a uniform access through something that is not a handle");
        };
        let element = kind
            .element
            .ok_or_else(|| marlin_shaderc_target::BackendError {
                message: "a uniform without a type".into(),
            })?;
        let inner = self.w.ty(element, true)?;
        let block = self.w.block("uniform", inner);
        let ptr = self.w.ptr_ty(crate::spirv::storage_class::Uniform, block);
        let handle = self.val(h)?;
        Ok(self.value(ptr, op::BufferPointerEXT, &[handle]))
    }

    /// The pointer to a buffer's block from its handle, and whether it is read-only. A byte
    /// buffer's block holds words.
    fn buffer_pointer(&mut self, h: Value) -> R<(u32, bool)> {
        use marlin_shaderc_ir::ResourceKind::{ReadOnlyBuffer, ReadOnlyByteBuffer};
        let TypeData::Handle(kind) = self.w.m.data(self.f.ty(h)).clone() else {
            return unsupported("a buffer access through something that is not a handle");
        };
        let block = if self.is_byte_buffer(h) {
            self.w.word_block()
        } else {
            let element = kind
                .element
                .ok_or_else(|| marlin_shaderc_target::BackendError {
                    message: "a buffer without an element type".into(),
                })?;
            self.w.buffer_block(element)?
        };
        let ptr = self
            .w
            .ptr_ty(crate::spirv::storage_class::StorageBuffer, block);
        let handle = self.val(h)?;
        let bp = self.value(ptr, op::BufferPointerEXT, &[handle]);
        let readonly = matches!(kind.kind, ReadOnlyBuffer | ReadOnlyByteBuffer);
        if readonly {
            self.w
                .decorate(bp, crate::spirv::decoration::NonWritable, &[]);
        }
        Ok((bp, readonly))
    }

    fn atomic(
        &mut self,
        a: AtomicOp,
        s: Scope,
        order: MemOrder,
        ops: &[Value],
        rt: Option<u32>,
    ) -> R<Option<u32>> {
        let ptr = self.val(ops[0])?;
        let sc = self.w.uint(scope_of(s)?);
        let sem_bits = match order {
            MemOrder::Relaxed => 0,
            _ => semantics::AcquireRelease,
        };
        let sem = self.w.uint(sem_bits);
        let mut vals = Vec::new();
        for &v in &ops[1..] {
            vals.push(self.val(v)?);
        }
        let rt = rt.unwrap_or(0);
        Ok(Some(match a {
            AtomicOp::Load => self.value(rt, op::AtomicLoad, &[ptr, sc, sem]),
            AtomicOp::Store => {
                self.emit(op::AtomicStore, &[ptr, sc, sem, vals[0]]);
                return Ok(None);
            }
            AtomicOp::CmpXchg => {
                // The IR has the comparator first; SPIR-V the new value.
                self.value(
                    rt,
                    op::AtomicCompareExchange,
                    &[ptr, sc, sem, sem, vals[1], vals[0]],
                )
            }
            other => {
                let opcode = match other {
                    AtomicOp::Add => op::AtomicIAdd,
                    AtomicOp::Sub => op::AtomicISub,
                    AtomicOp::And => op::AtomicAnd,
                    AtomicOp::Or => op::AtomicOr,
                    AtomicOp::Xor => op::AtomicXor,
                    AtomicOp::SMin => op::AtomicSMin,
                    AtomicOp::SMax => op::AtomicSMax,
                    AtomicOp::UMin => op::AtomicUMin,
                    AtomicOp::UMax => op::AtomicUMax,
                    _ => op::AtomicExchange,
                };
                self.value(rt, opcode, &[ptr, sc, sem, vals[0]])
            }
        }))
    }

    fn wave(&mut self, w: WaveOp, cluster: Option<u32>, ops: &[Value], rt: u32) -> R<u32> {
        let f = self.f;
        self.w.capability(cap::GroupNonUniform);
        let sub = self.w.uint(scope::Subgroup);
        let mut args = Vec::new();
        for &v in ops {
            args.push(self.val(v)?);
        }
        let is_bool = ops
            .first()
            .is_some_and(|&v| self.scalar_of(f.ty(v)) == Some(Scalar::I1));
        Ok(match w {
            WaveOp::Reduce(r) | WaveOp::Scan { op: r, .. } => {
                self.w.capability(cap::GroupNonUniformArithmetic);
                let group = match (w, cluster) {
                    (
                        WaveOp::Scan {
                            exclusive: true, ..
                        },
                        _,
                    ) => group_operation::ExclusiveScan,
                    (WaveOp::Scan { .. }, _) => group_operation::InclusiveScan,
                    (_, Some(_)) => {
                        self.w.capability(cap::GroupNonUniformClustered);
                        group_operation::ClusteredReduce
                    }
                    _ => group_operation::Reduce,
                };
                let opcode = reduce(r, is_bool);
                let mut operands = vec![sub, group, args[0]];
                if let Some(n) = cluster {
                    operands.push(self.w.uint(n));
                }
                self.value(rt, opcode, &operands)
            }
            WaveOp::Ballot => {
                self.w.capability(cap::GroupNonUniformBallot);
                self.value(rt, op::GroupNonUniformBallot, &[sub, args[0]])
            }
            WaveOp::Broadcast | WaveOp::Shuffle => {
                self.w.capability(cap::GroupNonUniformShuffle);
                self.value(rt, op::GroupNonUniformShuffle, &[sub, args[0], args[1]])
            }
            WaveOp::Quad(q) => {
                self.w.capability(cap::GroupNonUniformQuad);
                match q {
                    QuadOp::ReadLane => self.value(
                        rt,
                        op::GroupNonUniformQuadBroadcast,
                        &[sub, args[0], args[1]],
                    ),
                    other => {
                        let direction = self.w.uint(match other {
                            QuadOp::SwapX => 0,
                            QuadOp::SwapY => 1,
                            _ => 2,
                        });
                        self.value(rt, op::GroupNonUniformQuadSwap, &[sub, args[0], direction])
                    }
                }
            }
            WaveOp::Vote(v) => {
                self.w.capability(cap::GroupNonUniformVote);
                let opcode = match v {
                    VoteOp::Any => op::GroupNonUniformAny,
                    VoteOp::All => op::GroupNonUniformAll,
                    VoteOp::Equal => op::GroupNonUniformAllEqual,
                };
                self.value(rt, opcode, &[sub, args[0]])
            }
            WaveOp::IsFirst => self.value(rt, op::GroupNonUniformElect, &[sub]),
            WaveOp::LaneIndex | WaveOp::LaneCount => {
                let which = if w == WaveOp::LaneIndex {
                    builtin::SubgroupLocalInvocationId
                } else {
                    builtin::SubgroupSize
                };
                let var = self.w.builtin_input(which);
                self.value(rt, op::Load, &[var])
            }
            WaveOp::Match => return unsupported("`wave.match`"),
        })
    }
}

fn bits(s: Scalar) -> u64 {
    match s {
        Scalar::I1 => 1,
        Scalar::I16 | Scalar::F16 => 16,
        Scalar::I64 | Scalar::F64 => 64,
        _ => 32,
    }
}

fn float_bits(s: Scalar, x: f64) -> u64 {
    match s {
        Scalar::F16 => u64::from(marlin_shaderc_ir::num::f64_to_f16(x)),
        Scalar::F64 => x.to_bits(),
        _ => u64::from((x as f32).to_bits()),
    }
}

fn fcmp(c: FloatCmp) -> R<u32> {
    let name = c.name();
    Ok(match name {
        "oeq" => op::FOrdEqual,
        "one" => op::FOrdNotEqual,
        "olt" => op::FOrdLessThan,
        "ole" => op::FOrdLessThanEqual,
        "ogt" => op::FOrdGreaterThan,
        "oge" => op::FOrdGreaterThanEqual,
        "ueq" => op::FUnordEqual,
        "une" => op::FUnordNotEqual,
        "ult" => op::FUnordLessThan,
        "ule" => op::FUnordLessThanEqual,
        "ugt" => op::FUnordGreaterThan,
        "uge" => op::FUnordGreaterThanEqual,
        other => return unsupported(format!("the float comparison `{other}`")),
    })
}

fn scope_of(s: Scope) -> R<u32> {
    Ok(match s {
        Scope::Device => scope::Device,
        Scope::Group => scope::Workgroup,
        Scope::Wave => scope::Subgroup,
        Scope::Lane => return unsupported("a lane-scope memory operation"),
    })
}

fn reduce(r: ReduceOp, is_bool: bool) -> u32 {
    match r {
        ReduceOp::Add => op::GroupNonUniformIAdd,
        ReduceOp::FAdd => op::GroupNonUniformFAdd,
        ReduceOp::Mul => op::GroupNonUniformIMul,
        ReduceOp::FMul => op::GroupNonUniformFMul,
        ReduceOp::SMin => op::GroupNonUniformSMin,
        ReduceOp::SMax => op::GroupNonUniformSMax,
        ReduceOp::UMin => op::GroupNonUniformUMin,
        ReduceOp::UMax => op::GroupNonUniformUMax,
        ReduceOp::FMin => op::GroupNonUniformFMin,
        ReduceOp::FMax => op::GroupNonUniformFMax,
        ReduceOp::And if !is_bool => op::GroupNonUniformBitwiseAnd,
        ReduceOp::Or if !is_bool => op::GroupNonUniformBitwiseOr,
        ReduceOp::Xor if !is_bool => op::GroupNonUniformBitwiseXor,
        ReduceOp::And => op::GroupNonUniformLogicalAnd,
        ReduceOp::Or => op::GroupNonUniformLogicalOr,
        ReduceOp::Xor => op::GroupNonUniformLogicalXor,
    }
}

/// The SPIR-V built-in a module variable bound to `b` is, in `space` of an entry of `stage`.
pub(crate) fn builtin_of(b: Builtin, stage: Stage, space: Space) -> R<u32> {
    Ok(match b {
        Builtin::DispatchThreadId => builtin::GlobalInvocationId,
        Builtin::GroupId => builtin::WorkgroupId,
        Builtin::GroupThreadId => builtin::LocalInvocationId,
        Builtin::GroupIndex => builtin::LocalInvocationIndex,
        Builtin::VertexId => builtin::VertexIndex,
        // Less the base instance where it is loaded.
        Builtin::InstanceId => builtin::InstanceIndex,
        // A pixel's position is its window coordinate.
        Builtin::Position if stage == Stage::Pixel && space == Space::Input => builtin::FragCoord,
        Builtin::Position => builtin::Position,
        Builtin::IsFrontFace => builtin::FrontFacing,
        Builtin::SampleIndex => builtin::SampleId,
        Builtin::Depth => builtin::FragDepth,
        Builtin::PrimitiveId => builtin::PrimitiveId,
        Builtin::CullPrimitive => builtin::CullPrimitiveEXT,
        other => return unsupported(format!("the built-in `{}`", other.name())),
    })
}

impl Body<'_, '_> {
    /// The label jump `k` of block `b` goes to: its target's, or the block it goes through.
    fn target(&self, b: Block, k: usize, block: Block) -> u32 {
        self.via
            .get(&(b, k))
            .copied()
            .unwrap_or(self.labels[&block])
    }

    /// The blocks `b`'s jumps go through, each jumping on to the jump's target.
    fn through(&mut self, b: Block) {
        let f = self.f;
        let Some(t) = f.terminator(b) else { return };
        let jumps = f.insts[t].op.successors(f.operands(t).len());
        for (k, (target, _)) in jumps.into_iter().enumerate() {
            if let Some(&via) = self.via.get(&(b, k)) {
                let l = self.labels[&target];
                self.emit(op::Label, &[via]);
                self.emit(op::Branch, &[l]);
            }
        }
    }
}

/// Image operand bits (SPIR-V 3.14).
mod image_operands {
    pub const BIAS: u32 = 0x1;
    pub const LOD: u32 = 0x2;
    pub const GRAD: u32 = 0x4;
    pub const SAMPLE: u32 = 0x40;
}

impl Body<'_, '_> {
    fn texture_kind(&self, h: Value) -> R<marlin_shaderc_ir::ResKind> {
        match self.w.m.data(self.f.ty(h)) {
            TypeData::Handle(k) if k.kind.texture().is_some() => Ok(*k),
            _ => unsupported("a texture operation on something that is not a texture"),
        }
    }

    /// The operands after a texture operation's first `lead`, by argument (`grad`: two).
    fn tex_args(
        &mut self,
        ops: &[Value],
        lead: usize,
        targs: &[TexArg],
    ) -> R<Vec<(TexArg, Vec<u32>)>> {
        let mut out = Vec::new();
        let mut k = lead;
        for &t in targs {
            let n = if t == TexArg::Grad { 2 } else { 1 };
            let mut ids = Vec::new();
            for v in &ops[k..k + n] {
                ids.push(self.val(*v)?);
            }
            out.push((t, ids));
            k += n;
        }
        Ok(out)
    }

    /// A coordinate with the layer appended (`uv` and a layer as `(u, v, layer)`), as floats
    /// for sampling.
    fn with_layer(
        &mut self,
        coord: u32,
        coord_ty: Type,
        layer: Option<u32>,
        float: bool,
    ) -> R<u32> {
        let Some(layer) = layer else { return Ok(coord) };
        let (s, n) = match self.w.m.data(coord_ty) {
            TypeData::Vector(s, n) => (*s, *n),
            TypeData::Scalar(s) => (*s, 1),
            _ => return unsupported("a texture coordinate that is not a vector"),
        };
        let layer = if float {
            let f32t = self.w.scalar(Scalar::F32);
            self.value(f32t, op::ConvertUToF, &[layer])
        } else {
            layer
        };
        let t = self.w.vec_ty(s, n + 1);
        Ok(self.value(t, op::CompositeConstruct, &[coord, layer]))
    }

    /// A four-component texel narrowed to the texel type `rt` (`n` components).
    fn narrow(&mut self, v4: u32, n: u8, rt: u32) -> u32 {
        match n {
            4 => v4,
            1 => self.value(rt, op::CompositeExtract, &[v4, 0]),
            _ => {
                let mut ops = vec![v4, v4];
                ops.extend(0..n as u32);
                self.value(rt, op::VectorShuffle, &ops)
            }
        }
    }

    fn texel_shape(&self, kind: &marlin_shaderc_ir::ResKind) -> R<(Scalar, u8)> {
        let texel = kind
            .element
            .ok_or_else(|| marlin_shaderc_target::BackendError {
                message: "a texture without a texel type".into(),
            })?;
        match self.w.m.data(texel) {
            TypeData::Scalar(s) => Ok((*s, 1)),
            TypeData::Vector(s, n) => Ok((*s, *n)),
            _ => unsupported("a texel that is not a scalar or vector"),
        }
    }

    fn tex_sample(&mut self, ops: &[Value], targs: &[TexArg], rt: Option<u32>) -> R<u32> {
        use image_operands::*;
        let kind = self.texture_kind(ops[0])?;
        let image = self.val(ops[0])?;
        let sampler = self.val(ops[1])?;
        let image_t = self.w.image_ty(&kind)?;
        let si_t = self.w.sampled_image_ty(image_t);
        let si = self.value(si_t, op::SampledImage, &[image, sampler]);
        let args = self.tex_args(ops, 3, targs)?;
        let get = |t: TexArg| {
            args.iter()
                .find(|(a, _)| *a == t)
                .map(|(_, ids)| ids.clone())
        };
        let coord = self.val(ops[2])?;
        let coord = self.with_layer(
            coord,
            self.f.ty(ops[2]),
            get(TexArg::Layer).map(|v| v[0]),
            true,
        )?;
        let (s, n) = self.texel_shape(&kind)?;
        let v4 = self.w.vec_ty(s, 4);
        let (opcode, mask, extra) = if let Some(l) = get(TexArg::Lod) {
            (op::ImageSampleExplicitLod, Some(LOD), l)
        } else if let Some(g) = get(TexArg::Grad) {
            (op::ImageSampleExplicitLod, Some(GRAD), g)
        } else if let Some(b) = get(TexArg::Bias) {
            (op::ImageSampleImplicitLod, Some(BIAS), b)
        } else {
            (op::ImageSampleImplicitLod, None, Vec::new())
        };
        let mut operands = vec![si, coord];
        if let Some(m) = mask {
            operands.push(m);
            operands.extend(extra);
        }
        let r4 = self.value(v4, opcode, &operands);
        Ok(self.narrow(r4, n, rt.unwrap_or(0)))
    }

    fn tex_load(&mut self, ops: &[Value], targs: &[TexArg], rt: Option<u32>) -> R<u32> {
        use image_operands::*;
        let kind = self.texture_kind(ops[0])?;
        let shape = kind.kind.texture().expect("a texture");
        let image = self.val(ops[0])?;
        let args = self.tex_args(ops, 2, targs)?;
        let get = |t: TexArg| args.iter().find(|(a, _)| *a == t).map(|(_, ids)| ids[0]);
        let coord = self.val(ops[1])?;
        let coord = self.with_layer(coord, self.f.ty(ops[1]), get(TexArg::Layer), false)?;
        let (s, n) = self.texel_shape(&kind)?;
        let v4 = self.w.vec_ty(s, 4);
        let r4 = if shape.storage {
            self.w.capability(cap::StorageImageReadWithoutFormat);
            self.value(v4, op::ImageRead, &[image, coord])
        } else if shape.multisampled {
            let sample = get(TexArg::Sample).unwrap_or_else(|| self.w.uint(0));
            self.value(v4, op::ImageFetch, &[image, coord, SAMPLE, sample])
        } else {
            let mip = get(TexArg::Mip).unwrap_or_else(|| self.w.uint(0));
            self.value(v4, op::ImageFetch, &[image, coord, LOD, mip])
        };
        Ok(self.narrow(r4, n, rt.unwrap_or(0)))
    }

    fn tex_store(&mut self, ops: &[Value], targs: &[TexArg]) -> R<()> {
        let kind = self.texture_kind(ops[0])?;
        let image = self.val(ops[0])?;
        let args = self.tex_args(ops, 3, targs)?;
        let layer = args
            .iter()
            .find(|(a, _)| *a == TexArg::Layer)
            .map(|(_, ids)| ids[0]);
        let coord = self.val(ops[1])?;
        let coord = self.with_layer(coord, self.f.ty(ops[1]), layer, false)?;
        let (s, n) = self.texel_shape(&kind)?;
        let mut texel = self.val(ops[2])?;
        // Widened to four components, the rest 0.
        if n < 4 {
            let v4 = self.w.vec_ty(s, 4);
            let zero = self.w.lit(s, 1, 0);
            let mut parts = vec![texel];
            parts.extend(std::iter::repeat_n(zero, 4 - n as usize));
            texel = self.value(v4, op::CompositeConstruct, &parts);
        }
        self.w.capability(cap::StorageImageWriteWithoutFormat);
        self.emit(op::ImageWrite, &[image, coord, texel]);
        Ok(())
    }
}
