//! Flow checks (0004 §6, §9.1): definite assignment of locals and `out` parameters, every path
//! of a value-returning function ending in `return`, and `switch` sections not falling through.
//!
//! The analysis follows C#'s rules on the structured HIR, with the state of a condition split
//! into "when true" and "when false" (so `if (a && Try(out var v)) Use(v);` is fine) and
//! constant `true`/`false` conditions taken into account. A local is tracked in slots: one per
//! field of a struct, one per component of a vector, one otherwise; so `o.xyz = a; o.w = 1;`
//! assigns all of `o`, as C# allows for struct fields. A write through more than one step
//! (`s.inner.x = 1`) needs the first step assigned already. A lambda sees the enclosing locals
//! as they are where it is written; a local function sees them as assigned.

use std::collections::{HashMap, HashSet};

use marlin_front_hir_def::store::{
    Arg, BinaryOp, BindingId, BindingKind, Expr, ExprId, InterpolationPart, LambdaBody, Literal,
    LocalParam, Pat, PatId, Predefined, RefKind, Stmt, StmtId, TypeRef, UnaryOp,
};
use marlin_front_hir_def::{ItemId, ItemKind, ItemTreeQuery};
use marlin_lang_query::Cancelled;

use super::{Checker, Coverage};
use crate::infer::{InferLoc, Root, Step};
use crate::item::ItemTypesQuery;
use crate::ty::Ty;

/// Which slots are definitely assigned; `None` where the point cannot be reached.
type State = Option<Vec<bool>>;

fn meet(a: State, b: State) -> State {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(a), Some(b)) => Some(a.iter().zip(&b).map(|(x, y)| *x && *y).collect()),
    }
}

/// Where `break` and `continue` lead: the states they carry, innermost target last.
#[derive(Default)]
struct Jumps {
    breaks: Vec<Vec<State>>,
    /// Loops only: `continue` skips `switch`.
    continues: Vec<Vec<State>>,
}

pub(super) struct Flow<'c, 'a, 'b> {
    checker: &'c mut Checker<'a, 'b>,
    /// First slot and slot count of each binding.
    slots: HashMap<BindingId, (usize, usize)>,
    /// The fields of struct-typed locals, in slot order.
    fields: HashMap<BindingId, Vec<ItemId>>,
    total: usize,
    jumps: Jumps,
    /// The `out` parameters of the function being walked.
    outs: Vec<BindingId>,
    /// Locals already reported, so a missed assignment is reported once.
    reported: HashSet<BindingId>,
}

impl<'c, 'a, 'b> Flow<'c, 'a, 'b> {
    pub(super) fn new(checker: &'c mut Checker<'a, 'b>) -> Result<Self, Cancelled> {
        let mut flow = Flow {
            checker,
            slots: HashMap::new(),
            fields: HashMap::new(),
            total: 0,
            jumps: Jumps::default(),
            outs: Vec::new(),
            reported: HashSet::new(),
        };
        let bindings: Vec<BindingId> = flow
            .checker
            .body
            .store
            .bindings
            .iter()
            .map(|(b, _)| b)
            .collect();
        for b in bindings {
            let count = flow.slot_count(b)?;
            flow.slots.insert(b, (flow.total, count));
            flow.total += count;
        }
        Ok(flow)
    }

    fn slot_count(&mut self, b: BindingId) -> Result<usize, Cancelled> {
        // Local functions are visible in their whole block and never assigned.
        if self.checker.body.store.bindings[b].kind == BindingKind::LocalFunction {
            return Ok(0);
        }
        let ty = self
            .checker
            .infer
            .binding_types
            .get(b)
            .cloned()
            .unwrap_or(Ty::Error);
        if let Some((_, n)) = self.checker.vector_of(&ty)
            && let Some(n) = n.value()
        {
            return Ok(n.clamp(1, 4) as usize);
        }
        if let Ty::Adt { item, .. } = &ty
            && self.checker.builtins.kind(*item).is_none()
        {
            let tree = self.checker.cx.get::<ItemTreeQuery>(&item.file)?;
            if tree.items[item.local].kind == ItemKind::Struct {
                let fields: Vec<ItemId> = tree.items[item.local]
                    .members
                    .iter()
                    .filter(|&&m| {
                        let d = &tree.items[m];
                        d.kind == ItemKind::Field && !d.is_static && !d.is_const
                    })
                    .map(|&m| ItemId {
                        file: item.file,
                        local: m,
                    })
                    .collect();
                if !fields.is_empty() {
                    let n = fields.len();
                    self.fields.insert(b, fields);
                    return Ok(n);
                }
            }
        }
        Ok(1)
    }

    fn range(&self, b: BindingId) -> std::ops::Range<usize> {
        let (start, n) = self.slots.get(&b).copied().unwrap_or((0, 0));
        start..start + n
    }

    /// The slots that `step` right after binding `b` covers; `None` for all of them.
    fn step_slots(&self, b: BindingId, step: &Step) -> Option<Vec<usize>> {
        let (start, n) = self.slots.get(&b).copied()?;
        match step {
            Step::Field(item) => {
                let index = self.fields.get(&b)?.iter().position(|f| f == item)?;
                Some(vec![start + index])
            }
            Step::Swizzle(components) if n > 1 => Some(
                components
                    .iter()
                    .map(|&c| start + (c as usize).min(n - 1))
                    .collect(),
            ),
            _ => None,
        }
    }

    fn assign(&self, state: &mut State, b: BindingId) {
        if let Some(s) = state {
            for i in self.range(b) {
                s[i] = true;
            }
        }
    }

    fn assigned(state: &State, slots: &[usize]) -> bool {
        match state {
            None => true,
            Some(s) => slots.iter().all(|&i| s[i]),
        }
    }

    // ---- functions ----

    pub(super) fn run(mut self) -> Result<(), Cancelled> {
        let body = self.checker.body.clone();
        let types = self
            .checker
            .cx
            .get::<ItemTypesQuery>(&self.checker.owner.item)?;
        let params: Vec<(BindingId, bool)> = body
            .params
            .iter()
            .map(|&b| {
                let out = match body.store.bindings[b].kind {
                    BindingKind::Param(i) => {
                        types.params.get(i as usize).and_then(|p| p.ref_kind) == Some(RefKind::Out)
                    }
                    _ => false,
                };
                (b, out)
            })
            .collect();
        let mut entry: State = Some(vec![false; self.total]);
        for &(b, out) in &params {
            if !out {
                self.assign(&mut entry, b);
            }
        }
        if let Some(init) = body.constructor_initializer {
            self.expr(&mut entry, init)?;
        }
        let returns_value = self
            .checker
            .infer
            .body_ty
            .as_ref()
            .is_some_and(|t| *t != Ty::Void && !t.is_error());
        if let Some(root) = body.root {
            let outs = params.iter().filter(|p| p.1).map(|p| p.0).collect();
            self.function(entry, outs, root, returns_value, InferLoc::Owner)?;
        }
        Ok(())
    }

    /// Walks a function, lambda or local function body from `state`.
    fn function(
        &mut self,
        mut state: State,
        outs: Vec<BindingId>,
        body: LambdaBody,
        returns_value: bool,
        loc: InferLoc,
    ) -> Result<(), Cancelled> {
        let saved_jumps = std::mem::take(&mut self.jumps);
        let saved_outs = std::mem::replace(&mut self.outs, outs);
        match body {
            LambdaBody::Expr(e) => {
                self.expr(&mut state, e)?;
                self.check_outs(&state, loc);
            }
            LambdaBody::Block(s) => {
                self.stmt(&mut state, s)?;
                if state.is_some() {
                    self.check_outs(&state, loc.clone());
                    if returns_value {
                        self.checker.report(
                            loc,
                            "E1126",
                            "not every path of this function returns a value",
                        );
                    }
                }
            }
        }
        self.jumps = saved_jumps;
        self.outs = saved_outs;
        Ok(())
    }

    fn local_params(&self, state: &mut State, params: &[LocalParam]) -> Vec<BindingId> {
        let mut outs = Vec::new();
        for p in params {
            if p.modifier == Some(RefKind::Out) {
                outs.push(p.binding);
            } else {
                self.assign(state, p.binding);
            }
        }
        outs
    }

    fn check_outs(&mut self, state: &State, loc: InferLoc) {
        for b in self.outs.clone() {
            let slots: Vec<usize> = self.range(b).collect();
            if !Self::assigned(state, &slots) {
                let name = self.checker.body.store.bindings[b].name.clone();
                self.checker.report(
                    loc.clone(),
                    "E1133",
                    format!(
                        "`out` parameter `{name}` must be assigned before the function returns"
                    ),
                );
            }
        }
    }

    // ---- statements ----

    fn stmt(&mut self, state: &mut State, s: StmtId) -> Result<(), Cancelled> {
        let stmt = self.checker.body.store.stmts[s].clone();
        match stmt {
            Stmt::Missing | Stmt::Empty => {}
            Stmt::Expr(e) => self.expr(state, e)?,
            Stmt::Local { declarators, .. } => {
                for (b, init) in declarators {
                    if let Some(init) = init {
                        self.expr(state, init)?;
                        self.assign(state, b);
                    }
                }
            }
            Stmt::Block(stmts) => {
                for st in stmts {
                    self.stmt(state, st)?;
                }
            }
            Stmt::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                let (mut t, mut f) = self.cond(state.take(), cond)?;
                self.stmt(&mut t, then)?;
                if let Some(o) = otherwise {
                    self.stmt(&mut f, o)?;
                }
                *state = meet(t, f);
            }
            Stmt::While { cond, body } => {
                let (mut t, f) = self.cond(state.take(), cond)?;
                self.enter_loop();
                self.stmt(&mut t, body)?;
                let (breaks, _) = self.leave_loop();
                *state = breaks.into_iter().fold(f, meet);
            }
            Stmt::Do { body, cond } => {
                self.enter_loop();
                self.stmt(state, body)?;
                let (breaks, continues) = self.leave_loop();
                let at_cond = continues.into_iter().fold(state.take(), meet);
                let (_, f) = self.cond(at_cond, cond)?;
                *state = breaks.into_iter().fold(f, meet);
            }
            Stmt::For {
                init,
                cond,
                update,
                body,
            } => {
                for st in init {
                    self.stmt(state, st)?;
                }
                let (mut t, f) = match cond {
                    Some(c) => self.cond(state.take(), c)?,
                    None => (state.take(), None),
                };
                self.enter_loop();
                self.stmt(&mut t, body)?;
                let (breaks, continues) = self.leave_loop();
                let mut at_update = continues.into_iter().fold(t, meet);
                for u in update {
                    self.expr(&mut at_update, u)?;
                }
                *state = breaks.into_iter().fold(f, meet);
            }
            Stmt::Foreach {
                variable,
                iterable,
                body,
            } => {
                self.expr(state, iterable)?;
                let mut inner = state.clone();
                self.declare(&mut inner, variable);
                self.enter_loop();
                self.stmt(&mut inner, body)?;
                let (breaks, _) = self.leave_loop();
                *state = breaks.into_iter().fold(state.take(), meet);
            }
            Stmt::Switch {
                scrutinee,
                sections,
            } => {
                self.expr(state, scrutinee)?;
                let entry = state.clone();
                let has_default = sections
                    .iter()
                    .any(|s| s.labels.iter().any(Option::is_none));
                let exhaustive = has_default || {
                    let ty = self.checker.infer.type_of(scrutinee);
                    let unguarded: Vec<PatId> = sections
                        .iter()
                        .flat_map(|s| s.labels.iter().flatten())
                        .filter(|(_, guard)| guard.is_none())
                        .map(|(p, _)| *p)
                        .collect();
                    self.checker.coverage(&ty, &unguarded)? == Coverage::Complete
                };
                self.jumps.breaks.push(Vec::new());
                for section in &sections {
                    let mut inner = entry.clone();
                    for (pat, guard) in section.labels.iter().flatten() {
                        self.pat(&mut inner, *pat);
                        if let Some(g) = guard {
                            self.expr(&mut inner, *g)?;
                        }
                    }
                    for &st in &section.body {
                        self.stmt(&mut inner, st)?;
                    }
                    if inner.is_some() {
                        let at = section.body.last().copied().unwrap_or(s);
                        self.checker.report(
                            InferLoc::Stmt(at),
                            "E1127",
                            "a `switch` section must end in `break`, `continue` or `return`",
                        );
                    }
                }
                let breaks = self.jumps.breaks.pop().unwrap_or_default();
                let after = if exhaustive { None } else { entry };
                *state = breaks.into_iter().fold(after, meet);
            }
            Stmt::Break => {
                if let Some(frame) = self.jumps.breaks.last_mut() {
                    frame.push(state.clone());
                }
                *state = None;
            }
            Stmt::Continue => {
                if let Some(frame) = self.jumps.continues.last_mut() {
                    frame.push(state.clone());
                }
                *state = None;
            }
            Stmt::Return(value) => {
                if let Some(v) = value {
                    self.expr(state, v)?;
                }
                self.check_outs(state, InferLoc::Stmt(s));
                *state = None;
            }
            Stmt::LocalFunction(f) => {
                // Checked where it is declared, with the enclosing locals taken as assigned.
                let mut entry = state.as_ref().map(|s| vec![true; s.len()]);
                let outs = self.local_params(&mut entry, &f.params);
                let returns_value = !matches!(
                    self.checker.body.store.types[f.ret],
                    TypeRef::Predefined(Predefined::Void)
                );
                if let Some(body) = f.body {
                    self.function(entry, outs, body, returns_value, InferLoc::Stmt(s))?;
                }
            }
        }
        Ok(())
    }

    fn enter_loop(&mut self) {
        self.jumps.breaks.push(Vec::new());
        self.jumps.continues.push(Vec::new());
    }

    /// The states of the loop's `break`s and `continue`s.
    fn leave_loop(&mut self) -> (Vec<State>, Vec<State>) {
        let breaks = self.jumps.breaks.pop().unwrap_or_default();
        let continues = self.jumps.continues.pop().unwrap_or_default();
        (breaks, continues)
    }

    fn declare(&mut self, state: &mut State, e: ExprId) {
        match &self.checker.body.store.exprs[e] {
            Expr::DeclVar { binding, .. } => {
                let b = *binding;
                self.assign(state, b);
            }
            Expr::DeclTuple(bindings) => {
                for b in bindings.clone() {
                    self.assign(state, b);
                }
            }
            _ => {}
        }
    }

    // ---- expressions ----

    /// A condition: the states after it when it is true and when it is false.
    fn cond(&mut self, mut state: State, e: ExprId) -> Result<(State, State), Cancelled> {
        match self.checker.body.store.exprs[e].clone() {
            Expr::Literal(Literal::Bool(true)) => Ok((state, None)),
            Expr::Literal(Literal::Bool(false)) => Ok((None, state)),
            Expr::Unary {
                op: UnaryOp::Not,
                operand,
            } => {
                let (t, f) = self.cond(state, operand)?;
                Ok((f, t))
            }
            Expr::Binary {
                op: BinaryOp::And,
                lhs,
                rhs,
            } => {
                let (t, f) = self.cond(state, lhs)?;
                let (t2, f2) = self.cond(t, rhs)?;
                Ok((t2, meet(f, f2)))
            }
            Expr::Binary {
                op: BinaryOp::Or,
                lhs,
                rhs,
            } => {
                let (t, f) = self.cond(state, lhs)?;
                let (t2, f2) = self.cond(f, rhs)?;
                Ok((meet(t, t2), f2))
            }
            Expr::Is { expr, pat } => {
                // The pattern's variables are assigned only where it matched.
                self.expr(&mut state, expr)?;
                let mut t = state.clone();
                self.pat(&mut t, pat);
                Ok((t, state))
            }
            _ => {
                self.expr(&mut state, e)?;
                Ok((state.clone(), state))
            }
        }
    }

    /// Walks `e` in evaluation order: reads need their locals assigned; writes assign them.
    fn expr(&mut self, state: &mut State, e: ExprId) -> Result<(), Cancelled> {
        let expr = self.checker.body.store.exprs[e].clone();
        match expr {
            Expr::Path(_) => self.read(state, e),
            Expr::Member { receiver, .. } => {
                if self.checker.infer.paths.get(e).is_some() {
                    self.read(state, e);
                } else {
                    self.expr(state, receiver)?;
                }
            }
            Expr::Assign { op, target, value } => {
                if op.is_some() {
                    self.read(state, target);
                }
                self.expr(state, value)?;
                self.write(state, target)?;
            }
            Expr::Unary { op, operand } => {
                self.expr(state, operand)?;
                if matches!(
                    op,
                    UnaryOp::PreInc | UnaryOp::PreDec | UnaryOp::PostInc | UnaryOp::PostDec
                ) {
                    self.write(state, operand)?;
                }
            }
            Expr::Binary {
                op: BinaryOp::And | BinaryOp::Or,
                ..
            }
            | Expr::Is { .. } => {
                let (t, f) = self.cond(state.take(), e)?;
                *state = meet(t, f);
            }
            Expr::Call { callee, args } => {
                match self.checker.body.store.exprs[callee] {
                    // `local.Method(...)`: the recorded path is the receiver's, which is read.
                    Expr::Path(_) => self.read(state, callee),
                    _ => self.expr(state, callee)?,
                }
                self.args(state, &args)?;
            }
            Expr::Index { base, args } => {
                self.expr(state, base)?;
                self.args(state, &args)?;
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(state, lhs)?;
                self.expr(state, rhs)?;
            }
            Expr::Conditional {
                cond,
                then,
                otherwise,
            } => {
                let (mut t, mut f) = self.cond(state.take(), cond)?;
                self.expr(&mut t, then)?;
                self.expr(&mut f, otherwise)?;
                *state = meet(t, f);
            }
            Expr::Cast { expr, .. } | Expr::As { expr, .. } => self.expr(state, expr)?,
            Expr::Range { start, end } => {
                for x in [start, end].into_iter().flatten() {
                    self.expr(state, x)?;
                }
            }
            Expr::Switch { scrutinee, arms } => {
                self.expr(state, scrutinee)?;
                let entry = state.clone();
                let mut after = None;
                for arm in &arms {
                    let mut inner = entry.clone();
                    self.pat(&mut inner, arm.pat);
                    if let Some(g) = arm.guard {
                        let (t, _) = self.cond(inner, g)?;
                        inner = t;
                    }
                    self.expr(&mut inner, arm.expr)?;
                    after = meet(after, inner);
                }
                // A switch expression that is not exhaustive is an error of its own.
                *state = if arms.is_empty() { entry } else { after };
            }
            Expr::With { expr, inits } => {
                self.expr(state, expr)?;
                for i in inits {
                    self.expr(state, i.value)?;
                }
            }
            Expr::ObjectInit { inits, .. } => {
                for i in inits {
                    self.expr(state, i.value)?;
                }
            }
            Expr::New { args, inits, .. } => {
                if let Some(args) = args {
                    self.args(state, &args)?;
                }
                for i in inits.into_iter().flatten() {
                    self.expr(state, i.value)?;
                }
            }
            Expr::Throw(Some(x)) => {
                self.expr(state, x)?;
                *state = None;
            }
            Expr::Collection(items) => {
                for x in items {
                    self.expr(state, x)?;
                }
            }
            Expr::Tuple(items) => {
                for (_, x) in items {
                    self.expr(state, x)?;
                }
            }
            Expr::Interpolated(parts) => {
                for part in parts {
                    if let InterpolationPart::Hole {
                        expr, alignment, ..
                    } = part
                    {
                        self.expr(state, expr)?;
                        if let Some(a) = alignment {
                            self.expr(state, a)?;
                        }
                    }
                }
            }
            Expr::Lambda { params, body } => {
                let mut entry = state.clone();
                let outs = self.local_params(&mut entry, &params);
                let returns_value = match body {
                    LambdaBody::Block(b) => self.returns_value(b),
                    LambdaBody::Expr(_) => false,
                };
                self.function(entry, outs, body, returns_value, InferLoc::Expr(e))?;
            }
            Expr::DeclVar { .. } | Expr::DeclTuple(_) => self.declare(state, e),
            Expr::Missing
            | Expr::Literal(_)
            | Expr::Type(_)
            | Expr::Default(_)
            | Expr::This
            | Expr::Base
            | Expr::Typeof(_)
            | Expr::Sizeof(_)
            | Expr::Throw(None) => {}
        }
        Ok(())
    }

    /// Whether a lambda's block returns a value somewhere (outside nested functions).
    fn returns_value(&self, s: StmtId) -> bool {
        let store = &self.checker.body.store;
        match &store.stmts[s] {
            Stmt::Return(value) => value.is_some(),
            Stmt::Block(stmts) => stmts.iter().any(|&st| self.returns_value(st)),
            Stmt::If {
                then, otherwise, ..
            } => self.returns_value(*then) || otherwise.is_some_and(|o| self.returns_value(o)),
            Stmt::While { body, .. }
            | Stmt::Do { body, .. }
            | Stmt::For { body, .. }
            | Stmt::Foreach { body, .. } => self.returns_value(*body),
            Stmt::Switch { sections, .. } => sections
                .iter()
                .any(|sec| sec.body.iter().any(|&st| self.returns_value(st))),
            _ => false,
        }
    }

    fn args(&mut self, state: &mut State, args: &[Arg]) -> Result<(), Cancelled> {
        // Inputs are evaluated first; the call assigns its `out` arguments.
        for arg in args {
            if arg.modifier != Some(RefKind::Out) {
                self.expr(state, arg.expr)?;
            }
        }
        for arg in args {
            if arg.modifier == Some(RefKind::Out) {
                self.write(state, arg.expr)?;
            }
        }
        Ok(())
    }

    /// A read of the value that the path or member expression `e` names.
    fn read(&mut self, state: &State, e: ExprId) {
        let Some(path) = self.checker.infer.paths.get(e).cloned() else {
            return;
        };
        if let Root::Local(b) = path.root {
            self.read_local(state, b, path.steps.first(), e);
        }
    }

    fn read_local(&mut self, state: &State, b: BindingId, first: Option<&Step>, at: ExprId) {
        let slots = match first.and_then(|s| self.step_slots(b, s)) {
            Some(slots) => slots,
            None => self.range(b).collect(),
        };
        if Self::assigned(state, &slots) || !self.reported.insert(b) {
            return;
        }
        let name = self.checker.body.store.bindings[b].name.clone();
        self.checker.report(
            InferLoc::Expr(at),
            "E1132",
            format!("`{name}` may be read before it is assigned"),
        );
    }

    /// A write to the place `e`.
    fn write(&mut self, state: &mut State, e: ExprId) -> Result<(), Cancelled> {
        match self.checker.body.store.exprs[e].clone() {
            Expr::DeclVar { .. } | Expr::DeclTuple(_) => self.declare(state, e),
            Expr::Tuple(items) => {
                for (_, x) in items {
                    self.write(state, x)?;
                }
            }
            Expr::Index { base, args } => {
                self.expr(state, base)?;
                self.args(state, &args)?;
            }
            other => {
                let Some(path) = self.checker.infer.paths.get(e).cloned() else {
                    if let Expr::Member { receiver, .. } = other {
                        self.expr(state, receiver)?;
                    }
                    return Ok(());
                };
                let Root::Local(b) = path.root else {
                    return Ok(());
                };
                match path.steps.as_slice() {
                    [] => self.assign(state, b),
                    [first] => match self.step_slots(b, first) {
                        Some(slots) => {
                            if let Some(s) = state {
                                for i in slots {
                                    s[i] = true;
                                }
                            }
                        }
                        // A property setter needs the whole value.
                        None => self.read_local(state, b, None, e),
                    },
                    [first, ..] => self.read_local(state, b, Some(first), e),
                }
            }
        }
        Ok(())
    }

    /// Assigns the variables a pattern declares (where it matched).
    fn pat(&mut self, state: &mut State, p: PatId) {
        match self.checker.body.store.pats[p].clone() {
            Pat::Var(b) => self.assign(state, b),
            Pat::And(a, b) | Pat::Or(a, b) => {
                self.pat(state, a);
                self.pat(state, b);
            }
            Pat::Not(a) => self.pat(state, a),
            Pat::Property(items) => {
                for (_, sub) in items {
                    self.pat(state, sub);
                }
            }
            Pat::Positional(items) => {
                for sub in items {
                    self.pat(state, sub);
                }
            }
            Pat::Missing | Pat::Discard | Pat::Constant(_) | Pat::Relational { .. } => {}
        }
    }
}
