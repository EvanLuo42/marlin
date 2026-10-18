//! Functions: blocks, types, dominance and structured control flow (SPIR-V 2.11).

use std::collections::{HashMap, HashSet};

use marlin_shaderc_ir::cfg::{dominates, dominators};

use super::types::Ty;
use super::{Inst, Validator};
use crate::spirv::{op, storage_class as sc};

pub(crate) fn functions(v: &mut Validator) {
    let mut k = 0;
    while k < v.insts.len() {
        if v.insts[k].opcode != op::Function {
            k += 1;
            continue;
        }
        let end = (k..v.insts.len()).find(|&e| v.insts[e].opcode == op::FunctionEnd);
        let Some(end) = end else {
            v.error(format!("{}: no `OpFunctionEnd`", v.insts[k].name()));
            return;
        };
        let body: Vec<Inst> = v.insts[k..=end].to_vec();
        let mut f = Func {
            v,
            insts: body,
            blocks: Vec::new(),
            index: HashMap::new(),
            local: HashMap::new(),
            ret: 0,
            problems: Vec::new(),
        };
        f.run();
        let problems = std::mem::take(&mut f.problems);
        v.problems.extend(problems);
        k = end + 1;
    }
}

struct BlockInfo {
    label: u32,
    /// Positions (in the function's instructions) of its instructions after the label.
    insts: Vec<usize>,
    merge: Option<usize>,
    term: Option<usize>,
    succs: Vec<usize>,
}

struct Func<'v> {
    v: &'v Validator,
    insts: Vec<Inst>,
    blocks: Vec<BlockInfo>,
    /// Label id to block position.
    index: HashMap<u32, usize>,
    /// Results defined in the function: (block, position in the function).
    local: HashMap<u32, (usize, usize)>,
    ret: u32,
    problems: Vec<String>,
}

fn is_terminator(opcode: u32) -> bool {
    matches!(
        opcode,
        op::Branch
            | op::BranchConditional
            | op::Switch
            | op::Return
            | op::ReturnValue
            | op::Unreachable
            | op::EmitMeshTasksEXT
    )
}

impl Func<'_> {
    fn error(&mut self, m: impl Into<String>) {
        self.problems.push(m.into());
    }

    fn ty(&self, t: u32) -> Option<&Ty> {
        self.v.types.get(t)
    }

    /// The type of value `id` (a constant, module variable, or local result).
    fn type_of(&self, id: u32) -> Option<u32> {
        if let Some(&(_, k)) = self.local.get(&id) {
            return self.insts[k].result_type;
        }
        let &k = self.v.defs.get(&id)?;
        self.v.insts[k].result_type
    }

    fn run(&mut self) {
        let head = self.insts[0].clone();
        let fn_ty = head.ops[3];
        match self.ty(fn_ty).cloned() {
            Some(Ty::Function { ret, params }) => {
                self.ret = ret;
                if head.result_type != Some(ret) {
                    self.error(format!(
                        "{}: its result type is not its function type's",
                        head.name()
                    ));
                }
                let given = self.insts[1..]
                    .iter()
                    .take_while(|i| i.opcode == op::FunctionParameter)
                    .count();
                if given != params.len() {
                    self.error(format!(
                        "{}: {given} parameters for a type with {}",
                        head.name(),
                        params.len()
                    ));
                }
            }
            _ => self.error(format!("{}: its type is not a function type", head.name())),
        }
        if !self.blocks_of() {
            return;
        }
        self.cfg_and_dominance();
    }

    /// Splits the body into blocks; false if it is too broken to go on.
    fn blocks_of(&mut self) -> bool {
        let mut k = 1;
        while k < self.insts.len() && self.insts[k].opcode == op::FunctionParameter {
            let i = self.insts[k].clone();
            if let Some(r) = i.result {
                self.local.insert(r, (usize::MAX, k));
            }
            k += 1;
        }
        let last = self.insts.len() - 1;
        let mut current: Option<usize> = None;
        while k < last {
            let i = self.insts[k].clone();
            if i.opcode == op::Label {
                if let Some(c) = current
                    && self.blocks[c].term.is_none()
                {
                    let l = self.blocks[c].label;
                    self.error(format!("block %{l} ends without a terminator"));
                }
                let label = i.result.unwrap_or(0);
                self.index.insert(label, self.blocks.len());
                self.blocks.push(BlockInfo {
                    label,
                    insts: Vec::new(),
                    merge: None,
                    term: None,
                    succs: Vec::new(),
                });
                current = Some(self.blocks.len() - 1);
                k += 1;
                continue;
            }
            let Some(c) = current else {
                self.error(format!(
                    "{}: an instruction before the function's first label",
                    i.name()
                ));
                return false;
            };
            let b = &mut self.blocks[c];
            if b.term.is_some() {
                let l = b.label;
                self.error(format!("{}: after block %{l}'s terminator", i.name()));
                k += 1;
                continue;
            }
            if i.opcode == op::Phi && b.insts.iter().any(|&p| self.insts[p].opcode != op::Phi) {
                self.error(format!("{}: `OpPhi` comes first in its block", i.name()));
            }
            if i.opcode == op::Variable {
                let first = c == 0
                    && self.blocks[0]
                        .insts
                        .iter()
                        .all(|&p| self.insts[p].opcode == op::Variable);
                if !first {
                    self.error(format!(
                        "{}: variables come first in the function's first block",
                        i.name()
                    ));
                }
            }
            if let Some(r) = i.result {
                self.local.insert(r, (c, k));
            }
            let l = self.blocks[c].label;
            let has_merge = self.blocks[c].merge.is_some();
            if matches!(i.opcode, op::LoopMerge | op::SelectionMerge) {
                if has_merge {
                    self.error(format!("block %{l} has two merge instructions"));
                }
                self.blocks[c].merge = Some(k);
            } else if is_terminator(i.opcode) {
                self.blocks[c].term = Some(k);
            } else if has_merge {
                self.error(format!(
                    "block %{l}: the merge instruction comes right before the terminator"
                ));
            }
            self.blocks[c].insts.push(k);
            k += 1;
        }
        if let Some(c) = current
            && self.blocks[c].term.is_none()
        {
            let l = self.blocks[c].label;
            self.error(format!("block %{l} ends without a terminator"));
        }
        if self.blocks.is_empty() {
            let n = self.insts[0].name();
            self.error(format!("{n}: a function definition has blocks"));
            return false;
        }
        // Successors.
        for c in 0..self.blocks.len() {
            let Some(t) = self.blocks[c].term else {
                continue;
            };
            let i = self.insts[t].clone();
            let labels: Vec<u32> = match i.opcode {
                op::Branch => vec![i.ops[0]],
                op::BranchConditional => vec![i.ops[1], i.ops[2]],
                op::Switch => {
                    let mut out = vec![i.ops[1]];
                    out.extend(i.id_at.iter().skip(2).map(|&p| i.ops[p]));
                    out
                }
                _ => Vec::new(),
            };
            for l in labels {
                match self.index.get(&l) {
                    Some(&s) => self.blocks[c].succs.push(s),
                    None => self.error(format!(
                        "{}: %{l} is not a block of this function",
                        i.name()
                    )),
                }
            }
        }
        self.problems.is_empty()
    }

    fn cfg_and_dominance(&mut self) {
        let n = self.blocks.len();
        let succs: Vec<Vec<usize>> = self.blocks.iter().map(|b| b.succs.clone()).collect();
        let idom = dominators(n, 0, &succs);
        let mut preds = vec![Vec::new(); n];
        for (b, ss) in succs.iter().enumerate() {
            for &s in ss {
                preds[s].push(b);
            }
        }
        // Blocks after the blocks that dominate them.
        for b in 1..n {
            if let Some(d) = idom[b]
                && d >= b
            {
                let (l, dl) = (self.blocks[b].label, self.blocks[d].label);
                self.error(format!("block %{l} comes before %{dl}, which dominates it"));
            }
        }
        // Definitions dominate uses; phis name each predecessor once.
        for b in 0..n {
            if idom[b].is_none() {
                continue;
            }
            for &k in &self.blocks[b].insts.clone() {
                let i = self.insts[k].clone();
                if i.opcode == op::Phi {
                    self.phi(&i, b, &preds[b], &idom);
                    continue;
                }
                for id in i.ids() {
                    let Some(&(db, dk)) = self.local.get(&id) else {
                        continue;
                    };
                    if db == usize::MAX {
                        continue;
                    }
                    let ok = if db == b {
                        dk < k
                    } else {
                        dominates(&idom, db, b)
                    };
                    if !ok {
                        self.error(format!(
                            "{}: %{id} is used where its definition does not dominate",
                            i.name()
                        ));
                    }
                }
                self.typecheck(&i);
            }
        }
        self.structure(&succs, &preds, &idom);
    }

    fn phi(&mut self, i: &Inst, b: usize, preds: &[usize], idom: &[Option<usize>]) {
        let pairs: Vec<(u32, u32)> = i.ops[2..]
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| (c[0], c[1]))
            .collect();
        let mut named = HashSet::new();
        for &(value, label) in &pairs {
            let Some(&p) = self.index.get(&label) else {
                self.error(format!("{}: %{label} is not a block", i.name()));
                continue;
            };
            if !preds.contains(&p) {
                self.error(format!("{}: %{label} is not a predecessor", i.name()));
            }
            if !named.insert(p) {
                self.error(format!("{}: names predecessor %{label} twice", i.name()));
            }
            if self.type_of(value) != i.result_type {
                self.error(format!("{}: %{value} is not of the phi's type", i.name()));
            }
            if let Some(&(db, _)) = self.local.get(&value)
                && db != usize::MAX
                && idom[p].is_some()
                && !dominates(idom, db, p)
            {
                self.error(format!(
                    "{}: %{value} does not dominate the end of %{label}",
                    i.name()
                ));
            }
        }
        let mut unique: Vec<usize> = preds.to_vec();
        unique.sort_unstable();
        unique.dedup();
        if named.len() != unique.len() {
            let l = self.blocks[b].label;
            self.error(format!(
                "{}: block %{l} has {} predecessors, the phi names {}",
                i.name(),
                unique.len(),
                named.len()
            ));
        }
    }

    // ---- types of operands and results ----

    fn numeric(&self, t: u32) -> Option<(u32, Ty)> {
        self.v.types.component(t).map(|(n, c)| (n, c.clone()))
    }

    fn typecheck(&mut self, i: &Inst) {
        let rt = i.result_type;
        let ops: Vec<u32> = i.id_at.iter().map(|&p| i.ops[p]).collect();
        let types: Vec<Option<u32>> = ops.iter().map(|&o| self.type_of(o)).collect();
        let same = |a: Option<u32>, b: Option<u32>| a.is_some() && a == b;
        let name = i.name();
        match i.opcode {
            op::IAdd
            | op::ISub
            | op::IMul
            | op::UDiv
            | op::SDiv
            | op::UMod
            | op::SRem
            | op::BitwiseAnd
            | op::BitwiseOr
            | op::BitwiseXor => {
                let int = rt
                    .and_then(|t| self.numeric(t))
                    .is_some_and(|(_, c)| matches!(c, Ty::Int { .. }));
                if !int || !same(types[0], rt) || !same(types[1], rt) {
                    self.error(format!("{name}: integer operands and result of one type"));
                }
            }
            op::ShiftLeftLogical | op::ShiftRightLogical | op::ShiftRightArithmetic => {
                let int = rt
                    .and_then(|t| self.numeric(t))
                    .is_some_and(|(_, c)| matches!(c, Ty::Int { .. }));
                if !int || !same(types[0], rt) {
                    self.error(format!("{name}: an integer base of the result's type"));
                }
            }
            op::FAdd | op::FSub | op::FMul | op::FDiv | op::FRem => {
                let float = rt
                    .and_then(|t| self.numeric(t))
                    .is_some_and(|(_, c)| matches!(c, Ty::Float { .. }));
                if !float || !same(types[0], rt) || !same(types[1], rt) {
                    self.error(format!("{name}: float operands and result of one type"));
                }
            }
            op::SNegate | op::Not | op::FNegate => {
                if !same(types[0], rt) {
                    self.error(format!("{name}: the operand has the result's type"));
                }
            }
            op::IEqual
            | op::INotEqual
            | op::ULessThan
            | op::SLessThan
            | op::ULessThanEqual
            | op::SLessThanEqual
            | op::UGreaterThan
            | op::SGreaterThan
            | op::UGreaterThanEqual
            | op::SGreaterThanEqual
            | op::FOrdEqual
            | op::FOrdNotEqual
            | op::FOrdLessThan
            | op::FOrdLessThanEqual
            | op::FOrdGreaterThan
            | op::FOrdGreaterThanEqual
            | op::FUnordEqual
            | op::FUnordNotEqual
            | op::FUnordLessThan
            | op::FUnordLessThanEqual
            | op::FUnordGreaterThan
            | op::FUnordGreaterThanEqual
            | op::LogicalEqual
            | op::LogicalNotEqual
            | op::LogicalAnd
            | op::LogicalOr => {
                let width = types[0].and_then(|t| self.numeric(t)).map(|(n, _)| n);
                let result = rt.and_then(|t| self.numeric(t));
                let ok = matches!(&result, Some((n, Ty::Bool)) if Some(*n) == width)
                    && types[0] == types[1];
                if !ok {
                    self.error(format!(
                        "{name}: two operands of one type, a bool result as wide"
                    ));
                }
            }
            op::Select => {
                let width = rt.and_then(|t| self.numeric(t)).map(|(n, _)| n);
                let cond = types[0].and_then(|t| self.numeric(t));
                let cond_ok = matches!(&cond, Some((1, Ty::Bool)))
                    || matches!(&cond, Some((n, Ty::Bool)) if Some(*n) == width);
                if !cond_ok || !same(types[1], rt) || !same(types[2], rt) {
                    self.error(format!(
                        "{name}: a bool condition and objects of the result's type"
                    ));
                }
            }
            op::Load => {
                let pointee = types[0].and_then(|t| match self.ty(t) {
                    Some(Ty::Pointer { pointee, .. }) => Some(Some(*pointee)),
                    Some(Ty::Untyped { .. }) => Some(None),
                    _ => None,
                });
                match pointee {
                    None => self.error(format!("{name}: loads through a pointer")),
                    Some(Some(p)) if Some(p) != rt => {
                        self.error(format!("{name}: its type is not the pointer's"))
                    }
                    _ => {}
                }
            }
            op::Store => {
                let pointee = types[0].and_then(|t| match self.ty(t) {
                    Some(Ty::Pointer { pointee, .. }) => Some(Some(*pointee)),
                    Some(Ty::Untyped { .. }) => Some(None),
                    _ => None,
                });
                match pointee {
                    None => self.error(format!("{name}: stores through a pointer")),
                    Some(Some(p)) if Some(p) != types[1] => {
                        self.error(format!("{name}: the object's type is not the pointer's"))
                    }
                    _ => {}
                }
            }
            op::AccessChain => self.access_chain(i, &ops, &types),
            op::CompositeExtract => {
                let mut t = types[0];
                for &index in &i.ops[3..] {
                    t = t.and_then(|t| self.member(t, Some(index)));
                }
                if t != rt {
                    self.error(format!(
                        "{name}: the indices do not lead to the result's type"
                    ));
                }
            }
            op::CompositeInsert => {
                let mut t = types[1];
                for &index in &i.ops[4..] {
                    t = t.and_then(|t| self.member(t, Some(index)));
                }
                if t != types[0] || types[1] != rt {
                    self.error(format!(
                        "{name}: the object does not fit where the indices lead"
                    ));
                }
            }
            op::BranchConditional => {
                let cond = types[0].and_then(|t| self.ty(t).cloned());
                if cond != Some(Ty::Bool) {
                    self.error(format!("{name}: the condition is a bool"));
                }
            }
            op::Switch => {
                let s = types[0].and_then(|t| self.ty(t).cloned());
                if !matches!(s, Some(Ty::Int { .. })) {
                    self.error(format!("{name}: the selector is an integer"));
                }
            }
            op::ReturnValue => {
                if types[0] != Some(self.ret) {
                    self.error(format!(
                        "{name}: the value is not of the function's return type"
                    ));
                }
            }
            op::Return => {
                if self.ty(self.ret) != Some(&Ty::Void) {
                    self.error(format!(
                        "{name}: a function that returns a value returns one"
                    ));
                }
            }
            op::BufferPointerEXT => {
                let ok = types[0].and_then(|t| self.ty(t).cloned())
                    == Some(Ty::Untyped {
                        class: sc::UniformConstant,
                    });
                let result = rt.and_then(|t| self.ty(t).cloned());
                if !ok
                    || !matches!(
                        result,
                        Some(
                            Ty::Pointer {
                                class: sc::StorageBuffer | sc::Uniform,
                                ..
                            } | Ty::Untyped {
                                class: sc::StorageBuffer | sc::Uniform
                            }
                        )
                    )
                {
                    self.error(format!(
                        "{name}: a heap descriptor's untyped pointer in, a `StorageBuffer` or `Uniform` pointer out"
                    ));
                }
            }
            op::UntypedAccessChainKHR => {
                let base_is_heap = self
                    .v
                    .decorated(ops[1], crate::spirv::decoration::BuiltIn)
                    .is_some_and(|e| {
                        matches!(
                            e.first(),
                            Some(
                                &crate::spirv::builtin::ResourceHeapEXT
                                    | &crate::spirv::builtin::SamplerHeapEXT
                            )
                        )
                    });
                let descriptors = matches!(self.ty(ops[0]), Some(Ty::RuntimeArray { element }) if matches!(self.ty(*element), Some(Ty::Buffer { .. } | Ty::Image(_) | Ty::Sampler)));
                if base_is_heap && !descriptors {
                    self.error(format!(
                        "{name}: a heap is indexed as an array of descriptors"
                    ));
                }
            }
            op::Phi => {}
            _ => {}
        }
    }

    /// The type `index` (a member index, or any element for `None`) leads to inside `t`.
    fn member(&self, t: u32, index: Option<u32>) -> Option<u32> {
        match self.ty(t)? {
            Ty::Vector { component, count } => {
                index.is_none_or(|k| k < *count).then_some(*component)
            }
            Ty::Matrix { column, count } => index.is_none_or(|k| k < *count).then_some(*column),
            Ty::Array { element, length } => index.is_none_or(|k| k < *length).then_some(*element),
            Ty::RuntimeArray { element } => Some(*element),
            Ty::Struct(members) => members.get(index? as usize).copied(),
            _ => None,
        }
    }

    fn access_chain(&mut self, i: &Inst, ops: &[u32], types: &[Option<u32>]) {
        let name = i.name();
        let Some(Ty::Pointer { class, pointee }) = types[0].and_then(|t| self.ty(t).cloned())
        else {
            return self.error(format!("{name}: the base is a typed pointer"));
        };
        let mut t = Some(pointee);
        for &index in &ops[1..] {
            let is_struct = t.is_some_and(|t| matches!(self.ty(t), Some(Ty::Struct(_))));
            let constant = super::types::constant_value(self.v, index);
            if is_struct && constant.is_none() {
                return self.error(format!("{name}: a struct member is chosen by a constant"));
            }
            t = t.and_then(|t| self.member(t, if is_struct { constant } else { None }));
        }
        let result = i.result_type.and_then(|r| self.ty(r).cloned());
        let ok = matches!(result, Some(Ty::Pointer { class: c, pointee: p }) if c == class && Some(p) == t);
        if !ok {
            self.error(format!(
                "{name}: its type is not a pointer, in the base's class, to what the indices reach"
            ));
        }
    }

    // ---- structured control flow ----

    fn structure(&mut self, succs: &[Vec<usize>], preds: &[Vec<usize>], idom: &[Option<usize>]) {
        let n = self.blocks.len();
        // Headers and the structural edges they add.
        let mut structural: Vec<Vec<usize>> = succs.to_vec();
        let mut loops: Vec<(usize, usize, usize)> = Vec::new(); // header, merge, continue
        let mut selections: Vec<(usize, usize, bool)> = Vec::new(); // header, merge, is switch
        let mut merges: HashMap<usize, usize> = HashMap::new();
        let mut conts: HashMap<usize, usize> = HashMap::new();
        for b in 0..n {
            let Some(mk) = self.blocks[b].merge else {
                if let Some(t) = self.blocks[b].term
                    && matches!(self.insts[t].opcode, op::BranchConditional | op::Switch)
                {
                    let i = self.insts[t].clone();
                    self.error(format!(
                        "{}: selection must be structured (no merge instruction)",
                        i.name()
                    ));
                }
                continue;
            };
            let mi = self.insts[mk].clone();
            let Some(&m) = self.index.get(&mi.ops[0]) else {
                self.error(format!("{}: %{} is not a block", mi.name(), mi.ops[0]));
                continue;
            };
            if m == b {
                self.error(format!(
                    "{}: a header is not its own merge block",
                    mi.name()
                ));
            }
            if merges.insert(m, b).is_some() {
                let l = self.blocks[m].label;
                self.error(format!("%{l} is the merge block of two headers"));
            }
            structural[b].push(m);
            let term = self.blocks[b].term.map(|t| self.insts[t].clone());
            if mi.opcode == op::LoopMerge {
                let Some(&c) = self.index.get(&mi.ops[1]) else {
                    self.error(format!("{}: %{} is not a block", mi.name(), mi.ops[1]));
                    continue;
                };
                if c == m {
                    self.error(format!(
                        "{}: the continue target is not the merge block",
                        mi.name()
                    ));
                }
                if conts.insert(c, b).is_some() || merges.contains_key(&c) {
                    let l = self.blocks[c].label;
                    self.error(format!(
                        "%{l} is the continue target of two loops, or also a merge block"
                    ));
                }
                structural[b].push(c);
                loops.push((b, m, c));
                if let Some(t) = &term {
                    match t.opcode {
                        op::Switch => self.error(format!("{}: selection must be structured (a loop header's switch needs its own block)", t.name())),
                        op::BranchConditional => {
                            let targets = [self.index.get(&t.ops[1]).copied(), self.index.get(&t.ops[2]).copied()];
                            if !targets.contains(&Some(m)) && !targets.contains(&Some(c)) {
                                self.error(format!("{}: selection must be structured (a loop header's branch leaves for its merge or continue target)", t.name()));
                            }
                        }
                        _ => {}
                    }
                }
            } else {
                let is_switch = term.as_ref().is_some_and(|t| t.opcode == op::Switch);
                selections.push((b, m, is_switch));
            }
        }
        let sdom = dominators(n, 0, &structural);
        let sd = |a: usize, b: usize| sdom[b].is_some() && dominates(&sdom, a, b);
        // Constructs: (kind, header, merge, blocks).
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        enum Kind {
            Selection,
            Switch,
            Case,
            Loop,
            Continue,
        }
        let mut constructs: Vec<(Kind, usize, usize, Vec<bool>, usize)> = Vec::new(); // .., owner header
        for &(h, m, c) in &loops {
            let lp: Vec<bool> = (0..n).map(|b| sd(h, b) && !sd(m, b)).collect();
            let cont: Vec<bool> = (0..n).map(|b| sd(c, b) && !sd(m, b)).collect();
            constructs.push((Kind::Loop, h, m, lp, h));
            constructs.push((Kind::Continue, c, m, cont, h));
        }
        for &(h, m, is_switch) in &selections {
            let blocks: Vec<bool> = (0..n).map(|b| sd(h, b) && !sd(m, b)).collect();
            constructs.push((
                if is_switch {
                    Kind::Switch
                } else {
                    Kind::Selection
                },
                h,
                m,
                blocks,
                h,
            ));
            if is_switch {
                for &t in &succs[h] {
                    if t == m {
                        continue;
                    }
                    if !sd(h, t) {
                        let (l, tl) = (self.blocks[h].label, self.blocks[t].label);
                        self.error(format!(
                            "switch header %{l} does not structurally dominate its case %{tl}"
                        ));
                    }
                    let blocks: Vec<bool> = (0..n).map(|b| sd(t, b) && !sd(m, b)).collect();
                    constructs.push((Kind::Case, t, m, blocks, h));
                }
            }
        }
        let size = |c: &(Kind, usize, usize, Vec<bool>, usize)| c.3.iter().filter(|x| **x).count();
        // Loops: one back edge, from the continue construct.
        for &(h, _, c) in &loops {
            let back: Vec<usize> = preds[h].iter().copied().filter(|&p| sd(h, p)).collect();
            let l = self.blocks[h].label;
            // A continue target nothing reaches may have no back edge.
            if back.len() > 1 || (back.is_empty() && idom[c].is_some()) {
                self.error(format!(
                    "loop header %{l} is the target of {} back edges, not one",
                    back.len()
                ));
            }
            for p in back {
                if !sd(c, p) {
                    let pl = self.blocks[p].label;
                    self.error(format!("the back edge from %{pl} to loop header %{l} does not come from its continue construct"));
                }
            }
        }
        // Each edge leaves only the constructs it may leave.
        let innermost = |a: usize, kinds: &[Kind]| {
            constructs
                .iter()
                .enumerate()
                .filter(|(_, c)| kinds.contains(&c.0) && c.3[a])
                .min_by_key(|(_, c)| size(c))
                .map(|(k, _)| k)
        };
        // A loop's blocks are its loop and continue constructs together.
        let in_loop = |k: usize, b: usize| {
            let (kind, _, m, blocks, owner) = &constructs[k];
            blocks[b]
                || (*kind == Kind::Loop
                    && constructs
                        .iter()
                        .any(|c| c.0 == Kind::Continue && c.4 == *owner && c.2 == *m && c.3[b]))
        };
        let mut problems = Vec::new();
        for a in 0..n {
            if idom[a].is_none() {
                continue;
            }
            for &b in &succs[a] {
                for (k, c) in constructs.iter().enumerate() {
                    if !c.3[a] || c.3[b] {
                        continue;
                    }
                    // Leaving a loop construct for its own continue construct is still inside.
                    if c.0 == Kind::Loop && in_loop(k, b) {
                        continue;
                    }
                    let lp = innermost(a, &[Kind::Loop, Kind::Continue]).map(|k| constructs[k].4);
                    let lp = lp.and_then(|h| loops.iter().find(|l| l.0 == h).copied());
                    let any = innermost(
                        a,
                        &[
                            Kind::Selection,
                            Kind::Switch,
                            Kind::Case,
                            Kind::Loop,
                            Kind::Continue,
                        ],
                    );
                    let sw = innermost(a, &[Kind::Switch]);
                    let ok = (c.0 == Kind::Selection && any == Some(k) && b == c.2)
                        || lp.is_some_and(|(h, m, cont)| {
                            b == m || b == cont || (b == h && sd(cont, a))
                        })
                        || sw.is_some_and(|s| {
                            let (_, _, m, blocks, _) = &constructs[s];
                            let loop_inside_switch = lp.is_some_and(|(h, _, _)| blocks[h]);
                            b == *m && !loop_inside_switch
                        })
                        || (c.0 == Kind::Case
                            && constructs
                                .iter()
                                .any(|o| o.0 == Kind::Case && o.4 == c.4 && o.1 == b));
                    if !ok {
                        let kind = format!("{:?}", c.0).to_lowercase();
                        problems.push(format!(
                            "the branch from %{} to %{} leaves the {kind} construct at %{} for a block that is not its merge, a break or a continue",
                            self.blocks[a].label, self.blocks[b].label, self.blocks[c.1].label
                        ));
                    }
                }
                // Entering a construct is through its header.
                for c in &constructs {
                    if c.3[b] && !c.3[a] && b != c.1 {
                        let fallthrough = c.0 == Kind::Case
                            && (a == c.4
                                || constructs
                                    .iter()
                                    .any(|o| o.0 == Kind::Case && o.4 == c.4 && o.3[a]));
                        if !fallthrough {
                            let kind = format!("{:?}", c.0).to_lowercase();
                            problems.push(format!(
                                "the branch from %{} to %{} enters the {kind} construct at %{} other than through its header",
                                self.blocks[a].label, self.blocks[b].label, self.blocks[c.1].label
                            ));
                        }
                    }
                }
            }
        }
        problems.sort();
        problems.dedup();
        self.problems.extend(problems);
    }
}
