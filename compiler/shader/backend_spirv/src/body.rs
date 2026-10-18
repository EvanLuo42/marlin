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
    AccessStep, AtomicOp, Axis, BinaryOp, Block, Builtin, ConvertOp, Existing, FloatCmp, Function,
    Inst, IntCmp, MathOp, MemOrder, Op, QuadOp, ReduceOp, Scalar, Scope, Space, Stage, TexArg,
    Type, TypeData, UnaryOp, Value, ValueDef, VoteOp, WaveOp, scalar_field_offset, scalar_layout,
    step_type_in,
};

use crate::spirv::{builtin, capability as cap, glsl, group_operation, op, scope, semantics};
use crate::write::{Global, Member, R, Writer, inst, laid_out, storage_class, unsupported};

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
    /// The heap pointer each texture handle was loaded from, for texel pointers (atomics).
    image_ptrs: HashMap<Value, u32>,
    /// Pointers into laid-out memory that no SPIR-V pointer stands for.
    places: HashMap<Value, Place>,
}

/// A pointer into a laid-out struct's flattened members (see `write.rs`): what a load, store or
/// further access through it reaches.
#[derive(Clone, Copy, Debug)]
enum Place {
    /// A nested struct: members `first` on of the laid-out struct `base` points at.
    Flat { base: u32, first: u32 },
    /// Something inside an array of structs that a laid-out struct holds as words: `words`
    /// points at that member, and the place starts at word `at`.
    Words { words: u32, at: WordIndex },
}

/// A word index into a `Place::Words` member: a constant plus a computed part, if any.
#[derive(Clone, Copy, Debug, Default)]
struct WordIndex {
    dynamic: Option<u32>,
    fixed: u32,
}

impl WordIndex {
    fn plus(self, words: u64) -> Self {
        Self {
            fixed: self.fixed + words as u32,
            ..self
        }
    }
}

/// Where words come from: a `Place::Words` member through its pointer, or the member's value.
#[derive(Clone, Copy)]
enum WordSrc {
    Ptr { words: u32, class: u32 },
    Val(u32),
}

/// Where words go: through a member's pointer, or into the words of a value being built.
enum WordDst<'a> {
    Ptr { words: u32, class: u32 },
    Val(&'a mut [u32]),
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
            image_ptrs: HashMap::new(),
            places: HashMap::new(),
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
            Op::Access(steps) => self.access(&ops, steps, result.expect("a result"))?,
            Op::DescGet { nonuniform } => {
                let TypeData::Res(kind) = self.w.m.data(f.ty(ops[0])).clone() else {
                    return unsupported("`desc.get` of something that is not a resource");
                };
                use marlin_shaderc_ir::ResourceKind;
                let sampler = matches!(
                    kind.kind,
                    ResourceKind::Sampler | ResourceKind::ComparisonSampler
                );
                let index = a(self, 0)?;
                let uc = crate::spirv::storage_class::UniformConstant;
                let (p, loaded) = if kind.kind == ResourceKind::AccelerationStructure {
                    // Through the set 0 mapping, not `ResourceHeapEXT` (see `accel_heap`).
                    let heap = self.w.accel_heap();
                    let accel = self.w.accel_ty();
                    let ptr_t = self.w.ptr_ty(uc, accel);
                    (self.value(ptr_t, op::AccessChain, &[heap, index]), true)
                } else {
                    let array = self.w.heap_array(&kind)?;
                    let heap = if sampler {
                        self.w.sampler_heap()
                    } else {
                        self.w.heap()
                    };
                    let ptr_t = self.w.untyped_ptr_ty(uc);
                    let p = self.value(ptr_t, op::UntypedAccessChainKHR, &[array, heap, index]);
                    // Images and samplers are loaded; a buffer is reached through the pointer.
                    (p, kind.kind.texture().is_some() || sampler)
                };
                let v = if loaded {
                    self.value(rt.unwrap_or(0), op::Load, &[p])
                } else {
                    p
                };
                if kind.kind.texture().is_some()
                    && let Some(r) = result
                {
                    self.image_ptrs.insert(r, p);
                }
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
            Op::TexGather { channel, args } => Some(self.tex_gather(&ops, *channel, args)?),
            Op::TexQuery(q) => Some(self.tex_query(&ops, *q, rt)?),
            Op::RqInit => {
                // IR: query, accel, origin, t-min, direction, t-max, flags, mask; SPIR-V puts
                // the flags and mask before the ray.
                let mut ids = Vec::new();
                for k in 0..8 {
                    ids.push(a(self, k)?);
                }
                self.emit(
                    op::RayQueryInitializeKHR,
                    &[
                        ids[0], ids[1], ids[6], ids[7], ids[2], ids[3], ids[4], ids[5],
                    ],
                );
                None
            }
            Op::RqProceed => {
                let q = a(self, 0)?;
                Some(self.value(rt.unwrap_or(0), op::RayQueryProceedKHR, &[q]))
            }
            Op::RqCommitTriangle => {
                let q = a(self, 0)?;
                self.emit(op::RayQueryConfirmIntersectionKHR, &[q]);
                None
            }
            Op::RqGet { committed, field } => {
                let q = a(self, 0)?;
                Some(self.rq_get(q, *committed, *field, rt)?)
            }
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
            Op::TexAtomic {
                op: aop,
                scope: s,
                order,
                args,
            } => self.tex_atomic(*aop, *s, *order, &ops, args, rt)?,
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
            Op::Deriv { axis, fine } => {
                self.w.capability(cap::DerivativeControl);
                let opcode = match (axis, fine) {
                    (Axis::X, false) => op::DPdxCoarse,
                    (Axis::X, true) => op::DPdxFine,
                    (Axis::Y, false) => op::DPdyCoarse,
                    (Axis::Y, true) => op::DPdyFine,
                };
                let x = a(self, 0)?;
                Some(self.value(rt.unwrap_or(0), opcode, &[x]))
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
            Op::DebugPrint(fmt) => {
                self.debug_print(*fmt, &ops)?;
                None
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
        let (class, pointee) = self.pointer(p)?;
        if let Some(&place) = self.places.get(&p) {
            return self.load_place(place, class, pointee);
        }
        let ptr = self.val(p)?;
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
            self.plain_of(loaded, pointee)?
        } else {
            loaded
        })
    }

    fn store(&mut self, p: Value, v: Value, aligned: Option<u32>) -> R<()> {
        let (class, pointee) = self.pointer(p)?;
        let mut x = self.val(v)?;
        if let Some(&place) = self.places.get(&p) {
            return self.store_place(place, class, pointee, x);
        }
        let ptr = self.val(p)?;
        let laid = self.w.ty(pointee, laid_out(class))?;
        let plain = self.ty(v)?;
        if laid != plain {
            x = self.laid_of(x, pointee)?;
        }
        let mut operands = vec![ptr, x];
        if let Some(n) = aligned {
            operands.extend([0x2, n]);
        }
        self.emit(op::Store, &operands);
        Ok(())
    }

    /// `access`: an `OpAccessChain`, except where laid-out memory has no SPIR-V type for what it
    /// reaches (a nested struct, or a place inside an array of structs that a struct holds as
    /// words): that result is a [`Place`] and has no id.
    fn access(&mut self, ops: &[Value], steps: &[AccessStep], r: Value) -> R<Option<u32>> {
        let rt = self.ty(r)?;
        let (class, mut t) = self.pointer(ops[0])?;
        let mut indices = ops[1..].iter().copied();
        if !laid_out(class) {
            let mut operands = vec![self.val(ops[0])?];
            for s in steps {
                operands.push(match s {
                    AccessStep::Field(n) => self.w.uint(*n),
                    AccessStep::Index => self.val(indices.next().expect("an index"))?,
                });
            }
            return Ok(Some(self.value(rt, op::AccessChain, &operands)));
        }
        /// A pointer with the indices still to apply to it, or a place.
        enum At {
            Ptr(u32, Vec<u32>),
            Place(Place),
        }
        let mut at = match self.places.get(&ops[0]) {
            Some(&p) => At::Place(p),
            None => At::Ptr(self.val(ops[0])?, Vec::new()),
        };
        for &s in steps {
            let index = match s {
                AccessStep::Index => Some(indices.next().expect("an index")),
                AccessStep::Field(_) => None,
            };
            let next = step_type_in(&mut Existing(self.w.m), t, s)
                .map_err(|message| marlin_shaderc_target::BackendError { message })?;
            at = match (at, s) {
                // A field of a laid-out struct, or of a nested one flattened into it.
                (At::Ptr(ptr, mut chain), AccessStep::Field(n))
                    if matches!(self.w.m.data(t), TypeData::Struct(_))
                        && self.w.member(next) == Member::Plain =>
                {
                    chain.push(self.w.uint(self.w.flat_start(t, n)));
                    At::Ptr(ptr, chain)
                }
                (at @ (At::Ptr(..) | At::Place(Place::Flat { .. })), AccessStep::Field(n))
                    if matches!(self.w.m.data(t), TypeData::Struct(_)) =>
                {
                    let (base, first) = match at {
                        At::Ptr(ptr, chain) => (self.chain(ptr, chain, class, t)?, 0),
                        At::Place(Place::Flat { base, first }) => (base, first),
                        At::Place(Place::Words { .. }) => unreachable!(),
                    };
                    let m = first + self.w.flat_start(t, n);
                    match self.w.member(next) {
                        Member::Nested => At::Place(Place::Flat { base, first: m }),
                        Member::Words => At::Place(Place::Words {
                            words: self.member_ptr(base, m, class, next)?,
                            at: WordIndex::default(),
                        }),
                        Member::Plain => At::Ptr(base, vec![self.w.uint(m)]),
                    }
                }
                (At::Ptr(ptr, mut chain), _) => {
                    chain.push(match (index, s) {
                        (Some(i), _) => self.val(i)?,
                        (None, AccessStep::Field(n)) => self.w.uint(n),
                        (None, AccessStep::Index) => unreachable!(),
                    });
                    At::Ptr(ptr, chain)
                }
                (At::Place(Place::Words { words, at }), _) => {
                    // Words from the start of `t` to the start of `next`: per index, or fixed.
                    let (per, fixed) = match (self.w.m.data(t), s) {
                        (TypeData::Struct(_), AccessStep::Field(n)) => {
                            (0, self.field_offset(t, n)? / 4)
                        }
                        (TypeData::Array(..), _) => (self.stride(next)? / 4, 0),
                        (TypeData::Vector(e, _), _) => (scalar_bytes(*e) / 4, 0),
                        (TypeData::Matrix { element, rows, .. }, _) => {
                            (scalar_bytes(*element) * *rows as u64 / 4, 0)
                        }
                        _ => return unsupported("this access in an array of structs"),
                    };
                    let mut at = at.plus(fixed);
                    if per != 0 {
                        let i = self.index_u32(index.expect("an index step"))?;
                        let u32t = self.w.uint_ty(32);
                        let scaled = if per == 1 {
                            i
                        } else {
                            let k = self.w.uint(per as u32);
                            self.value(u32t, op::IMul, &[i, k])
                        };
                        at.dynamic = Some(match at.dynamic {
                            Some(d) => self.value(u32t, op::IAdd, &[d, scaled]),
                            None => scaled,
                        });
                    }
                    At::Place(Place::Words { words, at })
                }
                (At::Place(Place::Flat { .. }), _) => {
                    return unsupported("an index into a struct");
                }
            };
            t = next;
        }
        Ok(match at {
            At::Ptr(ptr, chain) if chain.is_empty() => Some(ptr),
            At::Ptr(ptr, chain) => {
                let mut operands = vec![ptr];
                operands.extend(chain);
                Some(self.value(rt, op::AccessChain, &operands))
            }
            At::Place(p) => {
                self.places.insert(r, p);
                None
            }
        })
    }

    /// `ptr` (into laid-out memory of `class`) with `chain` applied, which reaches a `t`.
    fn chain(&mut self, ptr: u32, chain: Vec<u32>, class: u32, t: Type) -> R<u32> {
        if chain.is_empty() {
            return Ok(ptr);
        }
        let laid = self.w.ty(t, true)?;
        let pt = self.w.ptr_ty(class, laid);
        let mut operands = vec![ptr];
        operands.extend(chain);
        Ok(self.value(pt, op::AccessChain, &operands))
    }

    /// A pointer to member `m` of the laid-out struct `base` points at, which field type `t`
    /// is laid out as.
    fn member_ptr(&mut self, base: u32, m: u32, class: u32, t: Type) -> R<u32> {
        let laid = match self.w.member(t) {
            Member::Words => self.w.words_ty(t)?,
            _ => self.w.ty(t, true)?,
        };
        let pt = self.w.ptr_ty(class, laid);
        let m = self.w.uint(m);
        Ok(self.value(pt, op::AccessChain, &[base, m]))
    }

    fn field_offset(&self, t: Type, n: u32) -> R<u64> {
        scalar_field_offset(self.w.m, t, n).ok_or_else(|| marlin_shaderc_target::BackendError {
            message: "a struct field without an offset".into(),
        })
    }

    /// The array stride of elements of type `t` in scalar layout.
    fn stride(&self, t: Type) -> R<u64> {
        let (size, align) =
            scalar_layout(self.w.m, t).ok_or_else(|| marlin_shaderc_target::BackendError {
                message: "a type without a memory layout in laid-out memory".into(),
            })?;
        Ok(size.div_ceil(align) * align)
    }

    /// Index `v` as a 32-bit integer.
    fn index_u32(&mut self, v: Value) -> R<u32> {
        let i = self.val(v)?;
        Ok(match self.scalar_of(self.f.ty(v)) {
            Some(Scalar::I32) | None => i,
            Some(_) => {
                let u32t = self.w.uint_ty(32);
                self.value(u32t, op::UConvert, &[i])
            }
        })
    }

    fn load_place(&mut self, place: Place, class: u32, t: Type) -> R<u32> {
        match place {
            Place::Flat { base, first } => self.load_flat(base, first, class, t),
            Place::Words { words, at } => self.read_words(WordSrc::Ptr { words, class }, at, t),
        }
    }

    fn store_place(&mut self, place: Place, class: u32, t: Type, x: u32) -> R<()> {
        match place {
            Place::Flat { base, first } => self.store_flat(base, first, class, t, x),
            Place::Words { words, at } => {
                self.write_words(&mut WordDst::Ptr { words, class }, at, t, x)
            }
        }
    }

    /// Loads struct `t` from members `first` on of the laid-out struct `base` points at.
    fn load_flat(&mut self, base: u32, first: u32, class: u32, t: Type) -> R<u32> {
        let mut parts = Vec::new();
        for (i, f) in self.fields(t).into_iter().enumerate() {
            let m = first + self.w.flat_start(t, i as u32);
            parts.push(match self.w.member(f) {
                Member::Nested => self.load_flat(base, m, class, f)?,
                Member::Words => {
                    let words = self.member_ptr(base, m, class, f)?;
                    self.read_words(WordSrc::Ptr { words, class }, WordIndex::default(), f)?
                }
                Member::Plain => {
                    let p = self.member_ptr(base, m, class, f)?;
                    let laid = self.w.ty(f, true)?;
                    let x = self.value(laid, op::Load, &[p]);
                    self.plain_of(x, f)?
                }
            });
        }
        let plain = self.w.ty(t, false)?;
        Ok(self.value(plain, op::CompositeConstruct, &parts))
    }

    /// Stores struct value `x` (of type `t`) to members `first` on of the laid-out struct
    /// `base` points at.
    fn store_flat(&mut self, base: u32, first: u32, class: u32, t: Type, x: u32) -> R<()> {
        for (i, f) in self.fields(t).into_iter().enumerate() {
            let m = first + self.w.flat_start(t, i as u32);
            let ft = self.w.ty(f, false)?;
            let e = self.value(ft, op::CompositeExtract, &[x, i as u32]);
            match self.w.member(f) {
                Member::Nested => self.store_flat(base, m, class, f, e)?,
                Member::Words => {
                    let words = self.member_ptr(base, m, class, f)?;
                    let mut dst = WordDst::Ptr { words, class };
                    self.write_words(&mut dst, WordIndex::default(), f, e)?;
                }
                Member::Plain => {
                    let p = self.member_ptr(base, m, class, f)?;
                    let e = self.laid_of(e, f)?;
                    self.emit(op::Store, &[p, e]);
                }
            }
        }
        Ok(())
    }

    fn fields(&self, t: Type) -> Vec<Type> {
        match self.w.m.data(t) {
            TypeData::Struct(s) => self.w.m.structs[*s].fields.iter().map(|f| f.1).collect(),
            _ => Vec::new(),
        }
    }

    /// Value `x` of `t`'s laid-out type as a plain value.
    fn plain_of(&mut self, x: u32, t: Type) -> R<u32> {
        let plain = self.w.ty(t, false)?;
        let laid = self.w.ty(t, true)?;
        if laid == plain {
            return Ok(x);
        }
        if !self.w.reshaped(t) {
            return Ok(self.value(plain, op::CopyLogical, &[x]));
        }
        match *self.w.m.data(t) {
            TypeData::Struct(_) => self.flat_to_plain(x, 0, t),
            TypeData::Array(e, n) => {
                let le = self.w.ty(e, true)?;
                let mut parts = Vec::new();
                for k in 0..n {
                    let x = self.value(le, op::CompositeExtract, &[x, k]);
                    parts.push(self.plain_of(x, e)?);
                }
                Ok(self.value(plain, op::CompositeConstruct, &parts))
            }
            _ => unreachable!("only structs and arrays are reshaped"),
        }
    }

    /// Struct `t` from members `first` on of laid-out struct value `x`.
    fn flat_to_plain(&mut self, x: u32, first: u32, t: Type) -> R<u32> {
        let mut parts = Vec::new();
        for (i, f) in self.fields(t).into_iter().enumerate() {
            let m = first + self.w.flat_start(t, i as u32);
            parts.push(match self.w.member(f) {
                Member::Nested => self.flat_to_plain(x, m, f)?,
                Member::Words => {
                    let wt = self.w.words_ty(f)?;
                    let words = self.value(wt, op::CompositeExtract, &[x, m]);
                    self.read_words(WordSrc::Val(words), WordIndex::default(), f)?
                }
                Member::Plain => {
                    let laid = self.w.ty(f, true)?;
                    let e = self.value(laid, op::CompositeExtract, &[x, m]);
                    self.plain_of(e, f)?
                }
            });
        }
        let plain = self.w.ty(t, false)?;
        Ok(self.value(plain, op::CompositeConstruct, &parts))
    }

    /// Plain value `x` of type `t` in `t`'s laid-out type.
    fn laid_of(&mut self, x: u32, t: Type) -> R<u32> {
        let plain = self.w.ty(t, false)?;
        let laid = self.w.ty(t, true)?;
        if laid == plain {
            return Ok(x);
        }
        if !self.w.reshaped(t) {
            return Ok(self.value(laid, op::CopyLogical, &[x]));
        }
        let mut parts = Vec::new();
        match *self.w.m.data(t) {
            TypeData::Struct(_) => self.plain_to_flat(x, t, &mut parts)?,
            TypeData::Array(e, n) => {
                let pe = self.w.ty(e, false)?;
                for k in 0..n {
                    let x = self.value(pe, op::CompositeExtract, &[x, k]);
                    parts.push(self.laid_of(x, e)?);
                }
            }
            _ => unreachable!("only structs and arrays are reshaped"),
        }
        Ok(self.value(laid, op::CompositeConstruct, &parts))
    }

    /// The laid-out members of plain struct value `x` of type `t`, appended to `out`.
    fn plain_to_flat(&mut self, x: u32, t: Type, out: &mut Vec<u32>) -> R<()> {
        for (i, f) in self.fields(t).into_iter().enumerate() {
            let ft = self.w.ty(f, false)?;
            let e = self.value(ft, op::CompositeExtract, &[x, i as u32]);
            match self.w.member(f) {
                Member::Nested => self.plain_to_flat(e, f, out)?,
                Member::Words => {
                    // Padding words are written as 0.
                    let (size, _) = scalar_layout(self.w.m, f).unwrap_or((0, 1));
                    let zero = self.w.uint(0);
                    let mut words = vec![zero; size.div_ceil(4) as usize];
                    self.write_words(&mut WordDst::Val(&mut words), WordIndex::default(), f, e)?;
                    let wt = self.w.words_ty(f)?;
                    out.push(self.value(wt, op::CompositeConstruct, &words));
                }
                Member::Plain => out.push(self.laid_of(e, f)?),
            }
        }
        Ok(())
    }

    /// The `t` at word `at` of `src`.
    fn read_words(&mut self, src: WordSrc, at: WordIndex, t: Type) -> R<u32> {
        let plain = self.w.ty(t, false)?;
        let parts = match *self.w.m.data(t) {
            TypeData::Scalar(s) => return self.read_scalar(src, at, s),
            TypeData::Res(_) => return self.read_scalar(src, at, Scalar::I32),
            TypeData::Vector(s, n) => {
                let mut parts = Vec::new();
                for k in 0..n as u64 {
                    let at = at.plus(k * scalar_bytes(s) / 4);
                    parts.push(self.read_scalar(src, at, s)?);
                }
                parts
            }
            TypeData::Matrix {
                element,
                rows,
                cols,
            } => {
                let e = self.w.scalar(element);
                let col = self.w.vector_ty(e, rows as u32);
                let mut parts = Vec::new();
                for c in 0..cols as u64 {
                    let mut comps = Vec::new();
                    for r in 0..rows as u64 {
                        let k = (c * rows as u64 + r) * scalar_bytes(element) / 4;
                        comps.push(self.read_scalar(src, at.plus(k), element)?);
                    }
                    parts.push(self.value(col, op::CompositeConstruct, &comps));
                }
                parts
            }
            TypeData::Struct(_) => {
                let mut parts = Vec::new();
                for (i, f) in self.fields(t).into_iter().enumerate() {
                    let offset = self.field_offset(t, i as u32)?;
                    parts.push(self.read_words(src, at.plus(offset / 4), f)?);
                }
                parts
            }
            TypeData::Array(e, n) => {
                let stride = self.stride(e)?;
                let mut parts = Vec::new();
                for k in 0..n as u64 {
                    parts.push(self.read_words(src, at.plus(k * stride / 4), e)?);
                }
                parts
            }
            _ => return unsupported("this type in an array of structs"),
        };
        Ok(self.value(plain, op::CompositeConstruct, &parts))
    }

    /// Stores `x` of type `t` at word `at` of `dst`.
    fn write_words(&mut self, dst: &mut WordDst, at: WordIndex, t: Type, x: u32) -> R<()> {
        match *self.w.m.data(t) {
            TypeData::Scalar(s) => self.write_scalar(dst, at, s, x),
            TypeData::Res(_) => self.write_scalar(dst, at, Scalar::I32, x),
            TypeData::Vector(s, n) => {
                let st = self.w.scalar(s);
                for k in 0..n as u32 {
                    let e = self.value(st, op::CompositeExtract, &[x, k]);
                    let at = at.plus(k as u64 * scalar_bytes(s) / 4);
                    self.write_scalar(dst, at, s, e)?;
                }
                Ok(())
            }
            TypeData::Matrix {
                element,
                rows,
                cols,
            } => {
                let st = self.w.scalar(element);
                for c in 0..cols as u32 {
                    for r in 0..rows as u32 {
                        let e = self.value(st, op::CompositeExtract, &[x, c, r]);
                        let k = (c * rows as u32 + r) as u64 * scalar_bytes(element) / 4;
                        self.write_scalar(dst, at.plus(k), element, e)?;
                    }
                }
                Ok(())
            }
            TypeData::Struct(_) => {
                for (i, f) in self.fields(t).into_iter().enumerate() {
                    let offset = self.field_offset(t, i as u32)?;
                    let ft = self.w.ty(f, false)?;
                    let e = self.value(ft, op::CompositeExtract, &[x, i as u32]);
                    self.write_words(dst, at.plus(offset / 4), f, e)?;
                }
                Ok(())
            }
            TypeData::Array(e, n) => {
                let stride = self.stride(e)?;
                let et = self.w.ty(e, false)?;
                for k in 0..n {
                    let v = self.value(et, op::CompositeExtract, &[x, k]);
                    self.write_words(dst, at.plus(k as u64 * stride / 4), e, v)?;
                }
                Ok(())
            }
            _ => unsupported("this type in an array of structs"),
        }
    }

    /// The word at `at` of `src`.
    fn word(&mut self, src: WordSrc, at: WordIndex) -> u32 {
        let u32t = self.w.uint_ty(32);
        match src {
            WordSrc::Ptr { words, class } => {
                let p = self.word_ptr(words, class, at);
                self.value(u32t, op::Load, &[p])
            }
            WordSrc::Val(v) => {
                debug_assert!(
                    at.dynamic.is_none(),
                    "a value's words are reached by constants"
                );
                self.value(u32t, op::CompositeExtract, &[v, at.fixed])
            }
        }
    }

    /// A pointer to word `at` of the words `words` points at.
    fn word_ptr(&mut self, words: u32, class: u32, at: WordIndex) -> u32 {
        let u32t = self.w.uint_ty(32);
        let pt = self.w.ptr_ty(class, u32t);
        let index = match at.dynamic {
            None => self.w.uint(at.fixed),
            Some(d) if at.fixed == 0 => d,
            Some(d) => {
                let k = self.w.uint(at.fixed);
                self.value(u32t, op::IAdd, &[d, k])
            }
        };
        self.value(pt, op::AccessChain, &[words, index])
    }

    fn read_scalar(&mut self, src: WordSrc, at: WordIndex, s: Scalar) -> R<u32> {
        let st = self.w.scalar(s);
        let w0 = self.word(src, at);
        Ok(match s {
            Scalar::I32 => w0,
            Scalar::F32 => self.value(st, op::Bitcast, &[w0]),
            Scalar::I1 => {
                let zero = self.w.uint(0);
                self.value(st, op::INotEqual, &[w0, zero])
            }
            Scalar::I64 | Scalar::F64 => {
                let w1 = self.word(src, at.plus(1));
                let u32t = self.w.uint_ty(32);
                let pair = self.w.vector_ty(u32t, 2);
                let v = self.value(pair, op::CompositeConstruct, &[w0, w1]);
                self.value(st, op::Bitcast, &[v])
            }
            Scalar::I16 | Scalar::F16 => {
                return unsupported("16-bit members of an array of structs inside a struct");
            }
        })
    }

    fn write_scalar(&mut self, dst: &mut WordDst, at: WordIndex, s: Scalar, x: u32) -> R<()> {
        let u32t = self.w.uint_ty(32);
        let words = match s {
            Scalar::I32 => vec![x],
            Scalar::F32 => vec![self.value(u32t, op::Bitcast, &[x])],
            Scalar::I1 => {
                let (one, zero) = (self.w.uint(1), self.w.uint(0));
                vec![self.value(u32t, op::Select, &[x, one, zero])]
            }
            Scalar::I64 | Scalar::F64 => {
                let pair = self.w.vector_ty(u32t, 2);
                let v = self.value(pair, op::Bitcast, &[x]);
                (0..2)
                    .map(|k| self.value(u32t, op::CompositeExtract, &[v, k]))
                    .collect()
            }
            Scalar::I16 | Scalar::F16 => {
                return unsupported("16-bit members of an array of structs inside a struct");
            }
        };
        for (k, w) in words.into_iter().enumerate() {
            let at = at.plus(k as u64);
            match dst {
                WordDst::Ptr { words, class } => {
                    let p = self.word_ptr(*words, *class, at);
                    self.emit(op::Store, &[p, w]);
                }
                WordDst::Val(out) => {
                    debug_assert!(
                        at.dynamic.is_none(),
                        "a value's words are reached by constants"
                    );
                    out[at.fixed as usize] = w;
                }
            }
        }
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
        let ty = match self.w.m.data(self.f.ty(ops[0])) {
            TypeData::Ptr { pointee, .. } => Some(*pointee),
            _ => None,
        };
        let ptr = match self.places.get(&ops[0]) {
            // A 32-bit integer is its word.
            Some(&Place::Words { words, at })
                if ty.is_some_and(|t| self.scalar_of(t) == Some(Scalar::I32)) =>
            {
                let (class, _) = self.pointer(ops[0])?;
                self.word_ptr(words, class, at)
            }
            Some(_) => {
                return unsupported(
                    "an atomic on an array of structs inside a struct, other than on a 32-bit \
                     integer",
                );
            }
            None => self.val(ops[0])?,
        };
        let mut vals = Vec::new();
        for &v in &ops[1..] {
            vals.push(self.val(v)?);
        }
        if ty.is_some_and(|t| self.scalar_of(t) == Some(Scalar::I64)) {
            self.w.capability(cap::Int64Atomics);
        }
        self.atomic_on(ptr, a, s, order, 0, &vals, rt.unwrap_or(0))
    }

    /// An atomic through `ptr` with the value ids `vals`; `memory` the semantics bit of what
    /// it points into, for orderings stronger than relaxed (0 for buffers and groupshared, as
    /// their barriers name them).
    #[allow(clippy::too_many_arguments)]
    fn atomic_on(
        &mut self,
        ptr: u32,
        a: AtomicOp,
        s: Scope,
        order: MemOrder,
        memory: u32,
        vals: &[u32],
        rt: u32,
    ) -> R<Option<u32>> {
        let sc = self.w.uint(scope_of(s)?);
        let sem_bits = match order {
            MemOrder::Relaxed => 0,
            _ => semantics::AcquireRelease | memory,
        };
        let sem = self.w.uint(sem_bits);
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

    /// `tex.atomic.*`: a texel pointer (`OpUntypedImageTexelPointerEXT` into the heap slot the
    /// texture was loaded from, with an image type that names the format atomics need), then
    /// the atomic. A signed texture's integers are signed for SPIR-V and the IR's on either
    /// side.
    fn tex_atomic(
        &mut self,
        a: AtomicOp,
        s: Scope,
        order: MemOrder,
        ops: &[Value],
        targs: &[TexArg],
        rt: Option<u32>,
    ) -> R<Option<u32>> {
        use crate::spirv::{image_format, storage_class as sc};
        let kind = self.texture_kind(ops[0])?;
        let Some(&heap_ptr) = self.image_ptrs.get(&ops[0]) else {
            return unsupported("a texture atomic on a texture not loaded from the heap");
        };
        let n = a.values();
        let args = self.tex_args(ops, 2 + n, targs)?;
        let layer = args
            .iter()
            .find(|(t, _)| *t == TexArg::Layer)
            .map(|(_, ids)| ids[0]);
        let coord = self.val(ops[1])?;
        let coord = self.with_layer(coord, self.f.ty(ops[1]), layer, false)?;
        let wide = self.w.texel_bits(&kind)? == 64;
        let format = match (wide, kind.signed) {
            (true, true) => image_format::R64i,
            (true, false) => image_format::R64ui,
            (false, true) => image_format::R32i,
            (false, false) => image_format::R32ui,
        };
        if wide {
            self.w.capability(cap::Int64Atomics);
        }
        let image_ty = self.w.image_ty_with(&kind, format)?;
        let ptr_t = self.w.untyped_ptr_ty(sc::Image);
        let sample = self.w.uint(0);
        let ptr = self.value(
            ptr_t,
            op::UntypedImageTexelPointerEXT,
            &[image_ty, heap_ptr, coord, sample],
        );
        let (s_ir, _) = self.texel_shape(&kind)?;
        let mut vals = Vec::new();
        for &v in &ops[2..2 + n] {
            vals.push(self.val(v)?);
        }
        let ir_t = self.w.scalar(s_ir);
        let image_t = self.w.image_scalar(&kind)?;
        if kind.signed {
            for v in &mut vals {
                *v = self.value(image_t, op::Bitcast, &[*v]);
            }
        }
        let image_memory = crate::spirv::semantics::ImageMemory;
        let r = self.atomic_on(ptr, a, s, order, image_memory, &vals, image_t)?;
        Ok(match r {
            Some(r) if kind.signed => Some(self.value(rt.unwrap_or(ir_t), op::Bitcast, &[r])),
            other => other,
        })
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
    pub const CONST_OFFSET: u32 = 0x8;
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

    /// Four components read from an image of `kind` as the IR's type (`s`): a signed texture's
    /// integers become the IR's signless ones.
    fn image_to_ir(&mut self, r4: u32, kind: &marlin_shaderc_ir::ResKind, s: Scalar) -> u32 {
        if !kind.signed {
            return r4;
        }
        let v4 = self.w.vec_ty(s, 4);
        self.value(v4, op::Bitcast, &[r4])
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

    /// Image operands in SPIR-V's order (by mask bit): the mask, then each one's operands.
    fn image_operands(parts: &[(u32, Vec<u32>)]) -> Vec<u32> {
        let mut parts: Vec<&(u32, Vec<u32>)> = parts.iter().collect();
        parts.sort_by_key(|(bit, _)| *bit);
        let mask = parts.iter().fold(0, |m, (bit, _)| m | bit);
        if mask == 0 {
            return Vec::new();
        }
        let mut out = vec![mask];
        for (_, ids) in parts {
            out.extend(ids);
        }
        out
    }

    /// For a sample or gather: the sampled image (texture and sampler operands), the
    /// coordinate with its layer, the texture's kind, and the extra operands by argument.
    #[allow(clippy::type_complexity)]
    fn sampling(
        &mut self,
        ops: &[Value],
        targs: &[TexArg],
    ) -> R<(
        u32,
        u32,
        marlin_shaderc_ir::ResKind,
        Vec<(TexArg, Vec<u32>)>,
    )> {
        let kind = self.texture_kind(ops[0])?;
        let image = self.val(ops[0])?;
        let sampler = self.val(ops[1])?;
        let image_t = self.w.image_ty(&kind)?;
        let si_t = self.w.sampled_image_ty(image_t);
        let si = self.value(si_t, op::SampledImage, &[image, sampler]);
        let args = self.tex_args(ops, 3, targs)?;
        let layer = args
            .iter()
            .find(|(a, _)| *a == TexArg::Layer)
            .map(|(_, ids)| ids[0]);
        let coord = self.val(ops[2])?;
        let coord = self.with_layer(coord, self.f.ty(ops[2]), layer, true)?;
        Ok((si, coord, kind, args))
    }

    fn tex_sample(&mut self, ops: &[Value], targs: &[TexArg], rt: Option<u32>) -> R<u32> {
        use image_operands::*;
        let (si, coord, kind, args) = self.sampling(ops, targs)?;
        let get = |t: TexArg| {
            args.iter()
                .find(|(a, _)| *a == t)
                .map(|(_, ids)| ids.clone())
        };
        let mut parts = Vec::new();
        let explicit = get(TexArg::Lod).is_some() || get(TexArg::Grad).is_some();
        for (t, bit) in [
            (TexArg::Bias, BIAS),
            (TexArg::Lod, LOD),
            (TexArg::Grad, GRAD),
            (TexArg::Offset, CONST_OFFSET),
        ] {
            if let Some(ids) = get(t) {
                parts.push((bit, ids));
            }
        }
        let operands = Self::image_operands(&parts);
        // A comparison: one float, the reference (`Dref`) before the image operands.
        if let Some(reference) = get(TexArg::Compare) {
            let opcode = if explicit {
                op::ImageSampleDrefExplicitLod
            } else {
                op::ImageSampleDrefImplicitLod
            };
            let f32t = self.w.scalar(Scalar::F32);
            let mut all = vec![si, coord, reference[0]];
            all.extend(operands);
            return Ok(self.value(f32t, opcode, &all));
        }
        let opcode = if explicit {
            op::ImageSampleExplicitLod
        } else {
            op::ImageSampleImplicitLod
        };
        let (s, n) = self.texel_shape(&kind)?;
        let v4 = self.w.image_texel4(&kind)?;
        let mut all = vec![si, coord];
        all.extend(operands);
        let r4 = self.value(v4, opcode, &all);
        let r4 = self.image_to_ir(r4, &kind, s);
        Ok(self.narrow(r4, n, rt.unwrap_or(0)))
    }

    /// `tex.gather`: one channel of four texels, or (with `compare`) four comparisons.
    fn tex_gather(&mut self, ops: &[Value], channel: u8, targs: &[TexArg]) -> R<u32> {
        use image_operands::*;
        let (si, coord, kind, args) = self.sampling(ops, targs)?;
        let get = |t: TexArg| {
            args.iter()
                .find(|(a, _)| *a == t)
                .map(|(_, ids)| ids.clone())
        };
        let parts: Vec<(u32, Vec<u32>)> = get(TexArg::Offset)
            .map(|o| (CONST_OFFSET, o))
            .into_iter()
            .collect();
        let operands = Self::image_operands(&parts);
        if let Some(reference) = get(TexArg::Compare) {
            let v4 = self.w.vec_ty(Scalar::F32, 4);
            let mut all = vec![si, coord, reference[0]];
            all.extend(operands);
            return Ok(self.value(v4, op::ImageDrefGather, &all));
        }
        let (s, _) = self.texel_shape(&kind)?;
        let v4 = self.w.image_texel4(&kind)?;
        let component = self.w.uint(channel as u32);
        let mut all = vec![si, coord, component];
        all.extend(operands);
        let r4 = self.value(v4, op::ImageGather, &all);
        Ok(self.image_to_ir(r4, &kind, s))
    }

    /// `tex.size`, `tex.layers`, `tex.mips` and `tex.samples`.
    fn tex_query(
        &mut self,
        ops: &[Value],
        q: marlin_shaderc_ir::TexQuery,
        rt: Option<u32>,
    ) -> R<u32> {
        use marlin_shaderc_ir::TexQuery;
        self.w.capability(cap::ImageQuery);
        let kind = self.texture_kind(ops[0])?;
        let shape = kind.kind.texture().expect("a texture");
        let image = self.val(ops[0])?;
        let u32t = self.w.scalar(Scalar::I32);
        match q {
            TexQuery::Mips => return Ok(self.value(u32t, op::ImageQueryLevels, &[image])),
            TexQuery::Samples => return Ok(self.value(u32t, op::ImageQuerySamples, &[image])),
            TexQuery::Size | TexQuery::Layers => {}
        }
        // The size of a level, with the layer count last (for a cube array, its cubes).
        let dims = if shape.cube { 2 } else { shape.dims };
        let n = dims + shape.array as u8;
        let all_t = self.w.vec_ty(Scalar::I32, n);
        let all = if shape.storage || shape.multisampled {
            self.value(all_t, op::ImageQuerySize, &[image])
        } else {
            let mip = match ops.get(1) {
                Some(&m) => self.val(m)?,
                None => self.w.uint(0),
            };
            self.value(all_t, op::ImageQuerySizeLod, &[image, mip])
        };
        let rt = rt.unwrap_or(0);
        Ok(match q {
            TexQuery::Layers => self.value(rt, op::CompositeExtract, &[all, dims as u32]),
            _ if !shape.array => all,
            _ => {
                let mut ops = vec![all, all];
                ops.extend(0..dims as u32);
                self.value(rt, op::VectorShuffle, &ops)
            }
        })
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
        let v4 = self.w.image_texel4(&kind)?;
        let mut parts = Vec::new();
        if let Some(o) = get(TexArg::Offset) {
            parts.push((CONST_OFFSET, vec![o]));
        }
        let r4 = if shape.storage {
            // 64-bit images name their format (`image_ty`).
            if self.w.texel_bits(&kind)? != 64 {
                self.w.capability(cap::StorageImageReadWithoutFormat);
            }
            self.value(v4, op::ImageRead, &[image, coord])
        } else {
            if shape.multisampled {
                let sample = get(TexArg::Sample).unwrap_or_else(|| self.w.uint(0));
                parts.push((SAMPLE, vec![sample]));
            } else {
                let mip = get(TexArg::Mip).unwrap_or_else(|| self.w.uint(0));
                parts.push((LOD, vec![mip]));
            }
            let mut all = vec![image, coord];
            all.extend(Self::image_operands(&parts));
            self.value(v4, op::ImageFetch, &all)
        };
        let r4 = self.image_to_ir(r4, &kind, s);
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
        if kind.signed {
            let image_v4 = self.w.image_texel4(&kind)?;
            texel = self.value(image_v4, op::Bitcast, &[texel]);
        }
        if self.w.texel_bits(&kind)? != 64 {
            self.w.capability(cap::StorageImageWriteWithoutFormat);
        }
        self.emit(op::ImageWrite, &[image, coord, texel]);
        Ok(())
    }
}

impl Body<'_, '_> {
    /// A field of the committed (or candidate) intersection of query `q`.
    fn rq_get(
        &mut self,
        q: u32,
        committed: bool,
        field: marlin_shaderc_ir::RqField,
        rt: Option<u32>,
    ) -> R<u32> {
        use marlin_shaderc_ir::RqField::*;
        let which = self.w.uint(committed as u32);
        let rt = rt.unwrap_or(0);
        let opcode = match field {
            Kind => {
                let kind = self.value(rt, op::RayQueryGetIntersectionTypeKHR, &[q, which]);
                // Committed: 0 none, 1 triangle, 2 procedural, as `HitKind`; a candidate is
                // 0 triangle, 1 procedural.
                return Ok(if committed {
                    kind
                } else {
                    let one = self.w.uint(1);
                    self.value(rt, op::IAdd, &[kind, one])
                });
            }
            T => op::RayQueryGetIntersectionTKHR,
            Barycentrics => op::RayQueryGetIntersectionBarycentricsKHR,
            InstanceIndex => op::RayQueryGetIntersectionInstanceIdKHR,
            InstanceId => op::RayQueryGetIntersectionInstanceCustomIndexKHR,
            PrimitiveIndex => op::RayQueryGetIntersectionPrimitiveIndexKHR,
            GeometryIndex => op::RayQueryGetIntersectionGeometryIndexKHR,
            FrontFace => op::RayQueryGetIntersectionFrontFaceKHR,
        };
        Ok(self.value(rt, opcode, &[q, which]))
    }
}

impl Body<'_, '_> {
    /// `debug.print`: `NonSemantic.DebugPrintf` with the format in printf's terms. Each hole of
    /// the format (`{u}`, `{i}`, `{f}`, `{b}`, 0006 §8.10) becomes `%u`, `%d`, `%f` (`%vN…` for
    /// vectors, `%l…` for 64 bits); bools print as 0 or 1.
    fn debug_print(&mut self, fmt: u32, ops: &[Value]) -> R<()> {
        let f = self.f;
        let format = self.w.m.formats.get(fmt as usize).cloned().ok_or_else(|| {
            marlin_shaderc_target::BackendError {
                message: format!("no format {fmt}"),
            }
        })?;
        let mut text = String::new();
        let mut args = Vec::new();
        let mut next = ops.iter();
        let mut chars = format.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    text.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    text.push('}');
                }
                '%' => text.push_str("%%"),
                '{' => {
                    let kind = chars.next();
                    chars.next(); // `}`
                    let Some(&v) = next.next() else {
                        return unsupported("a format with more holes than values");
                    };
                    let t = f.ty(v);
                    let (s, n) = match self.w.m.data(t) {
                        TypeData::Scalar(s) => (*s, 1),
                        TypeData::Vector(s, n) => (*s, *n),
                        _ => return unsupported("printing something not a scalar or vector"),
                    };
                    let mut id = self.val(v)?;
                    if s == Scalar::I1 {
                        let ut = self.w.vec_ty(Scalar::I32, n);
                        let (one, zero) =
                            (self.w.lit(Scalar::I32, n, 1), self.w.lit(Scalar::I32, n, 0));
                        id = self.value(ut, op::Select, &[id, one, zero]);
                    }
                    if n > 1 {
                        text.push_str(&format!("%v{n}"));
                    } else {
                        text.push('%');
                    }
                    if matches!(s, Scalar::I64 | Scalar::F64) {
                        text.push('l');
                    }
                    text.push(match kind {
                        Some('i') => 'd',
                        Some('f') => 'f',
                        _ => 'u',
                    });
                    args.push(id);
                }
                c => text.push(c),
            }
        }
        let set = self.w.printf();
        let string = self.w.op_string(&text);
        let void = self.w.void();
        let mut operands = vec![set, 1, string];
        operands.extend(args);
        self.value(void, op::ExtInst, &operands);
        Ok(())
    }
}

/// The bytes a scalar takes in laid-out memory (a bool takes a word).
fn scalar_bytes(s: Scalar) -> u64 {
    match s {
        Scalar::I16 | Scalar::F16 => 2,
        Scalar::I64 | Scalar::F64 => 8,
        _ => 4,
    }
}
