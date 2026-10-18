//! One function instance: its signature, and the context that lowers its body.
//!
//! - Parameters: `in` and plain parameters by value; `out` and `ref` as pointers to a variable
//!   of the caller (who copies in and out, 0004 §6).
//! - Instance methods of structs take `this` first: by value in `readonly` members, by pointer
//!   otherwise. Extension members take the receiver by value. Constructors take no `this`:
//!   they start from the field defaults and return the value they built.
//! - Field and enum member initializers lower to functions without parameters that return the
//!   value (compile-time evaluation runs them).

use std::collections::HashMap;
use std::sync::Arc;

use marlin_front_hir_def::store::{
    BindingId, BindingKind, Expr, ExprId, LambdaBody, RefKind, Stmt,
};
use marlin_front_hir_def::{Body, BodyOwner, BodyPart, BodyQuery, ItemKind, ItemTreeQuery};
use marlin_shaderc_hir_ty::{
    InferLoc, InferQuery, InferenceResult, ItemTypesQuery, Subst, Ty, self_ty,
};
use marlin_shaderc_ir::{
    Block, FuncId, Function, Inst, Op, Space, SwitchCase, Type, Value, result_types,
};

use crate::closures::{Capture, Inner, func_result};
use crate::ssa::{Ssa, Var};
use crate::{Instance, LowerError, Lowering, R};

/// How a parameter is passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pass {
    Value,
    /// A pointer to the caller's variable.
    Pointer,
}

/// A function instance's signature, in the front end's terms.
pub(crate) struct Signature {
    /// `this` (or an extension's receiver) first, if there is one: its type and how it passes.
    pub this: Option<(Ty, Pass)>,
    pub params: Vec<(Ty, Pass)>,
    /// `None` for `void`.
    pub result: Option<Ty>,
    pub is_constructor: bool,
}

pub(crate) fn signature(l: &mut Lowering<'_, '_>, instance: &Instance) -> R<Signature> {
    if let Some(inner) = &instance.inner {
        return inner_signature(l, instance, inner);
    }
    let item = instance.owner.item;
    let tree = l.cx.get::<ItemTreeQuery>(&item.file)?;
    let data = &tree.items[item.local];
    let types = l.cx.get::<ItemTypesQuery>(&item)?;
    let subst = &instance.subst;
    let parent = data.parent.map(|p| (p, &tree.items[p]));
    let mut sig = Signature {
        this: None,
        params: Vec::new(),
        result: None,
        is_constructor: data.kind == ItemKind::Constructor,
    };
    // Property and indexer accessors: a getter returns the value, a setter takes it last.
    let accessor = matches!(data.kind, ItemKind::Property | ItemKind::Indexer);
    let mut setter = false;
    match (data.kind, instance.owner.part) {
        (ItemKind::Field | ItemKind::EnumMember, _) | (_, BodyPart::Initializer) => {
            sig.result = types.ty.as_ref().map(|t| l.resolve_assoc(&subst.ty(t)));
            return Ok(sig);
        }
        (
            ItemKind::Function | ItemKind::Operator | ItemKind::Conversion | ItemKind::Constructor,
            _,
        ) => {}
        (ItemKind::Property | ItemKind::Indexer, _) => {
            let body = l.cx.get::<BodyQuery>(&instance.owner)?;
            setter = body
                .params
                .iter()
                .any(|&b| body.store.bindings[b].kind == BindingKind::SetterValue);
        }
        (kind, part) => {
            return Err(LowerError::new(
                "E1301",
                format!("lowering the {part:?} body of a {kind:?} is not done yet"),
            ));
        }
    }
    if let Some((p, pdata)) = parent {
        let parent_id = marlin_front_hir_def::ItemId {
            file: item.file,
            local: p,
        };
        let member = data.kind == ItemKind::Function || accessor;
        if pdata.kind.is_type() && !data.is_static && member {
            let this = l.resolve_assoc(&subst.ty(&self_ty(l.cx, parent_id)?));
            let pass = if (data.is_readonly || pdata.is_readonly || accessor) && !setter {
                Pass::Value
            } else {
                Pass::Pointer
            };
            sig.this = Some((this, pass));
        }
        if pdata.kind == ItemKind::Extension {
            let ext = l.cx.get::<ItemTypesQuery>(&parent_id)?;
            if let Some(r) = &ext.receiver {
                sig.this = Some((l.resolve_assoc(&subst.ty(r)), Pass::Value));
            }
        }
    }
    for p in &types.params {
        let pass = match p.ref_kind {
            Some(RefKind::Out | RefKind::Ref) => Pass::Pointer,
            _ => Pass::Value,
        };
        sig.params.push((l.resolve_assoc(&subst.ty(&p.ty)), pass));
    }
    if accessor {
        let value = types.ty.as_ref().map(|t| l.resolve_assoc(&subst.ty(t)));
        if setter {
            sig.params.extend(value.map(|v| (v, Pass::Value)));
        } else {
            sig.result = value;
        }
        return Ok(sig);
    }
    sig.result = if sig.is_constructor {
        types.ty.as_ref().map(|t| l.resolve_assoc(&subst.ty(t)))
    } else {
        types
            .ty
            .as_ref()
            .map(|t| l.resolve_assoc(&subst.ty(t)))
            .filter(|t| *t != Ty::Void)
    };
    Ok(sig)
}

/// A lambda's or local function's parameters and result (its environment comes before them).
fn inner_signature(l: &mut Lowering<'_, '_>, instance: &Instance, inner: &Inner) -> R<Signature> {
    let body = l.cx.get::<BodyQuery>(&instance.owner)?;
    let infer = l.cx.get::<InferQuery>(&instance.owner)?;
    let concrete = |l: &Lowering<'_, '_>, t: &Ty| l.resolve_assoc(&instance.subst.ty(t));
    let (params, result) = match inner {
        Inner::Lambda(e) => {
            let Expr::Lambda { params, .. } = &body.store.exprs[*e] else {
                return Err(LowerError::new("E1305", "not a lambda"));
            };
            let func = concrete(l, &infer.type_of(*e));
            (params.clone(), func_result(l, &func))
        }
        Inner::LocalFn { stmt, ret } => {
            let Stmt::LocalFunction(f) = &body.store.stmts[*stmt] else {
                return Err(LowerError::new("E1305", "not a local function"));
            };
            if !f.generic_params.is_empty() {
                return Err(LowerError::new(
                    "E1301",
                    "generic local functions are not lowered yet",
                ));
            }
            (f.params.clone(), ret.clone())
        }
        Inner::Method { .. } => {
            return Err(LowerError::new(
                "E1305",
                "a method group has no function of its own",
            ));
        }
        Inner::Const(b) => (
            Vec::new(),
            Some(concrete(
                l,
                &infer.binding_types.get(*b).cloned().unwrap_or(Ty::Error),
            )),
        ),
    };
    let mut sig = Signature {
        this: None,
        params: Vec::new(),
        result,
        is_constructor: false,
    };
    for p in params {
        let ty = concrete(
            l,
            &infer
                .binding_types
                .get(p.binding)
                .cloned()
                .unwrap_or(Ty::Error),
        );
        let pass = match p.modifier {
            Some(RefKind::Out | RefKind::Ref) => Pass::Pointer,
            _ => Pass::Value,
        };
        sig.params.push((ty, pass));
    }
    Ok(sig)
}

fn ir_param(l: &mut Lowering<'_, '_>, (ty, pass): &(Ty, Pass)) -> R<Type> {
    let t = l.ty(ty)?;
    Ok(match pass {
        Pass::Value => t,
        Pass::Pointer => l.module.ptr(Space::Function, false, t),
    })
}

/// Declares the IR function of an instance: name, parameters (on its entry block), results.
pub(crate) fn declare(l: &mut Lowering<'_, '_>, instance: &Instance) -> R<FuncId> {
    declare_signature(l, instance).map_err(|e| e.located(instance.owner, InferLoc::Owner))
}

fn declare_signature(l: &mut Lowering<'_, '_>, instance: &Instance) -> R<FuncId> {
    let sig = signature(l, instance)?;
    let mut params = Vec::new();
    // A lambda or local function takes its environment first.
    if instance
        .inner
        .as_ref()
        .is_some_and(|i| !matches!(i, Inner::Const(_)))
    {
        params.push(l.env_type(instance)?);
    }
    if let Some(this) = &sig.this {
        params.push(ir_param(l, this)?);
    }
    for (i, p) in sig.params.iter().enumerate() {
        // A `Func` parameter holds the environment of the lambda it is bound to.
        if crate::closures::is_func(l, &p.0) {
            let closure = instance
                .closures
                .iter()
                .find(|(k, _)| *k as usize == i)
                .map(|(_, c)| c.clone())
                .ok_or_else(|| {
                    LowerError::new("E1301", "a `Func` parameter without a lambda bound to it")
                })?;
            params.push(l.env_type(&closure)?);
            continue;
        }
        params.push(ir_param(l, p)?);
    }
    let results = match &sig.result {
        Some(t) => vec![l.ty(t)?],
        None => Vec::new(),
    };
    let mut f = Function::new("", results);
    let entry = f.add_block();
    for &t in &params {
        f.add_param(entry, t, None);
    }
    let id = l.module.add_function(f);
    let name = l.function_name(instance, &params, id)?;
    l.module.funcs[id].name = name.into();
    Ok(id)
}

/// A terminator before its jump arguments are known.
#[derive(Clone, Debug)]
pub(crate) enum Term {
    Br(Block),
    CondBr {
        cond: Value,
        then_block: Block,
        else_block: Block,
        merge: Block,
    },
    Switch {
        value: Value,
        cases: Vec<(u64, Block)>,
        default: Block,
        merge: Block,
    },
    Loop {
        header: Block,
        continuing: Block,
        merge: Block,
    },
    Return(Vec<Value>),
    Unreachable,
}

impl Term {
    fn targets(&self) -> Vec<Block> {
        match self {
            Term::Br(b) => vec![*b],
            Term::CondBr {
                then_block,
                else_block,
                ..
            } => vec![*then_block, *else_block],
            Term::Switch { cases, default, .. } => {
                cases.iter().map(|c| c.1).chain([*default]).collect()
            }
            Term::Loop { header, .. } => vec![*header],
            Term::Return(_) | Term::Unreachable => Vec::new(),
        }
    }
}

/// Where a local lives.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Local {
    Var(Var),
    /// Behind a pointer: `out` and `ref` parameters.
    Ptr(Value),
}

/// Where `break` and `continue` go.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Jumps {
    pub brk: Block,
    /// `None` for a `switch` (whose `continue` belongs to the loop around it).
    pub cont: Option<Block>,
}

pub(crate) struct FnCx<'l, 'a, 'b> {
    pub l: &'l mut Lowering<'a, 'b>,
    pub instance: Instance,
    pub f: Function,
    pub body: Arc<Body>,
    pub infer: Arc<InferenceResult>,
    pub entry: Block,
    pub block: Option<Block>,
    pub ssa: Ssa,
    pub terms: HashMap<Block, Term>,
    pub locals: HashMap<BindingId, Local>,
    /// `this`: a variable (by value, constructors) or the pointer parameter.
    pub this: Option<(Local, Ty)>,
    pub jumps: Vec<Jumps>,
    pub results: Option<Ty>,
    pub is_constructor: bool,
    /// The source location of what is being lowered, which new instructions get.
    pub loc: Option<u32>,
    /// Locals declared `[Uniform]`: what is written to them is asserted the same in every lane.
    pub uniform_locals: std::collections::HashSet<BindingId>,
}

pub(crate) fn lower_body(l: &mut Lowering<'_, '_>, instance: &Instance, id: FuncId) -> R<()> {
    let owner = instance.owner;
    let body = l.cx.get::<BodyQuery>(&owner)?;
    let infer = l.cx.get::<InferQuery>(&owner)?;
    let sig = signature(l, instance)?;
    // Work on the function outside the module; a stand-in keeps its name and results.
    let stand_in = Function::new(&l.module.funcs[id].name, l.module.funcs[id].results.clone());
    let f = std::mem::replace(&mut l.module.funcs[id], stand_in);
    let entry = f.entry().expect("declared with an entry block");
    let mut cx = FnCx {
        l,
        instance: instance.clone(),
        f,
        body: body.clone(),
        infer,
        entry,
        block: Some(entry),
        ssa: Ssa::default(),
        terms: HashMap::new(),
        locals: HashMap::new(),
        this: None,
        jumps: Vec::new(),
        results: sig.result.clone(),
        is_constructor: sig.is_constructor,
        loc: None,
        uniform_locals: std::collections::HashSet::new(),
    };
    cx.ssa.seal(&mut cx.f, &mut cx.l.module, entry);
    let result = cx.lower(&sig);
    let FnCx { l, f, .. } = cx;
    l.module.funcs[id] = f;
    result
}

impl FnCx<'_, '_, '_> {
    pub(crate) fn owner(&self) -> BodyOwner {
        self.instance.owner
    }

    pub(crate) fn subst(&self) -> &Subst {
        &self.instance.subst
    }

    /// An error at `loc` in this body.
    pub(crate) fn error(
        &self,
        code: &'static str,
        message: impl Into<String>,
        loc: InferLoc,
    ) -> LowerError {
        LowerError::at(code, message, self.owner(), loc)
    }

    /// A front-end type of this body made concrete.
    pub(crate) fn concrete(&self, ty: &Ty) -> Ty {
        self.l.resolve_assoc(&self.subst().ty(ty))
    }

    pub(crate) fn binding_ty(&self, b: BindingId) -> Ty {
        self.concrete(
            &self
                .infer
                .binding_types
                .get(b)
                .cloned()
                .unwrap_or(Ty::Error),
        )
    }

    fn lower(&mut self, sig: &Signature) -> R<()> {
        if let Some(inner) = self.instance.inner.clone() {
            return self.lower_inner(sig, &inner);
        }
        let params: Vec<Value> = self.f.params().to_vec();
        let mut next = 0;
        if let Some((ty, pass)) = &sig.this {
            let v = params[next];
            next += 1;
            let local = match pass {
                Pass::Pointer => Local::Ptr(v),
                Pass::Value => {
                    let t = self.f.ty(v);
                    let var = self.ssa.new_var(t, Some("this"));
                    self.ssa.write(var, self.entry, v);
                    Local::Var(var)
                }
            };
            self.this = Some((local, ty.clone()));
            self.f.set_name(v, Some("this"));
        }
        // The body's parameter bindings, in declaration order.
        let bindings: Vec<BindingId> = self.body.params.clone();
        let mut declared = Vec::new();
        for &b in &bindings {
            match self.body.store.bindings[b].kind {
                BindingKind::Param(i) => declared.push((i as usize, b)),
                // A setter's `value` comes after the indexer's parameters.
                BindingKind::SetterValue => declared.push((sig.params.len().saturating_sub(1), b)),
                BindingKind::Receiver => {
                    if let Some((local, _)) = self.this {
                        self.locals.insert(b, local);
                    }
                }
                _ => {}
            }
        }
        declared.sort_unstable_by_key(|(i, _)| *i);
        for (i, b) in declared {
            let Some(&v) = params.get(next + i) else {
                continue;
            };
            let name = self.body.store.bindings[b].name.to_string();
            self.f.set_name(v, Some(&name));
            let local = match sig.params.get(i).map(|p| p.1) {
                Some(Pass::Pointer) => Local::Ptr(v),
                _ => {
                    let t = self.f.ty(v);
                    let var = self.ssa.new_var(t, Some(&name));
                    self.ssa.write(var, self.entry, v);
                    Local::Var(var)
                }
            };
            self.locals.insert(b, local);
        }
        if self.is_constructor {
            let ty = sig.result.clone().unwrap_or(Ty::Error);
            let start = self.default_value(&ty, InferLoc::Owner)?;
            let t = self.l.ty(&ty)?;
            let var = self.ssa.new_var(t, Some("this"));
            self.write_var(var, start);
            self.this = Some((Local::Var(var), ty));
        }
        if let Some(init) = self.body.constructor_initializer {
            // `: this(...)` builds the value the body continues from.
            let v = self.value(init)?;
            if let Some((Local::Var(var), _)) = self.this {
                self.write_var(var, v);
            }
        }
        let root = self.body.root;
        self.lower_root(root)
    }

    /// A lambda or local function: its environment, then its parameters.
    fn lower_inner(&mut self, sig: &Signature, inner: &Inner) -> R<()> {
        if let Inner::Const(b) = inner {
            let init = local_initializer(&self.body, *b);
            return self.lower_root(init.map(LambdaBody::Expr));
        }
        let params: Vec<Value> = self.f.params().to_vec();
        let env = params[0];
        self.f.set_name(env, Some("env"));
        let captures = self.l.captures(&self.instance)?;
        for (k, c) in captures.into_iter().enumerate() {
            let v = self.op(Op::ExtractField(k as u32), vec![env], None)?;
            let t = self.f.ty(v);
            let var = self.ssa.new_var(t, None);
            self.write_var(var, v);
            match c {
                Capture::This => {
                    let outer = Instance {
                        inner: None,
                        ..self.instance.clone()
                    };
                    let outer_sig = signature(self.l, &outer)?;
                    let ty = match (outer_sig.this, outer_sig.is_constructor) {
                        (Some((t, _)), _) => t,
                        (None, _) => outer_sig.result.unwrap_or(Ty::Error),
                    };
                    self.this = Some((Local::Var(var), ty));
                }
                Capture::Binding(b) => {
                    self.locals.insert(b, Local::Var(var));
                }
            }
        }
        let (bindings, root): (Vec<(BindingId, Option<RefKind>)>, Option<LambdaBody>) = match inner
        {
            Inner::Lambda(e) => match &self.body.store.exprs[*e] {
                Expr::Lambda { params, body } => (
                    params.iter().map(|p| (p.binding, p.modifier)).collect(),
                    Some(*body),
                ),
                _ => (Vec::new(), None),
            },
            Inner::LocalFn { stmt, .. } => match &self.body.store.stmts[*stmt] {
                Stmt::LocalFunction(f) => (
                    f.params.iter().map(|p| (p.binding, p.modifier)).collect(),
                    f.body,
                ),
                _ => (Vec::new(), None),
            },
            Inner::Method { .. } | Inner::Const(_) => (Vec::new(), None),
        };
        for (i, (b, _)) in bindings.into_iter().enumerate() {
            let Some(&v) = params.get(1 + i) else {
                continue;
            };
            let name = self.body.store.bindings[b].name.to_string();
            self.f.set_name(v, Some(&name));
            let local = match sig.params.get(i).map(|p| p.1) {
                Some(Pass::Pointer) => Local::Ptr(v),
                _ => {
                    let t = self.f.ty(v);
                    let var = self.ssa.new_var(t, Some(&name));
                    self.write_var(var, v);
                    Local::Var(var)
                }
            };
            self.locals.insert(b, local);
        }
        self.lower_root(root)
    }

    fn lower_root(&mut self, root: Option<LambdaBody>) -> R<()> {
        match root {
            Some(LambdaBody::Expr(e)) => {
                let v = self.expr(e)?;
                if self.block.is_some() {
                    let vals = match (&self.results, v) {
                        (Some(_), Some(v)) => vec![v],
                        _ => Vec::new(),
                    };
                    self.ret(vals)?;
                }
            }
            Some(LambdaBody::Block(s)) => {
                self.stmt(s)?;
                if self.block.is_some() {
                    // Falling off the end: `void` functions and constructors return here.
                    let vals = self.implicit_return()?;
                    self.ret(vals)?;
                }
            }
            None => {
                return Err(self.error("E1301", "a function without a body", InferLoc::Owner));
            }
        }
        self.finish()
    }

    fn implicit_return(&mut self) -> R<Vec<Value>> {
        if self.is_constructor
            && let Some((Local::Var(var), _)) = self.this
        {
            return Ok(vec![self.read_var(var)]);
        }
        Ok(Vec::new())
    }

    /// `return`: constructors return the value they built.
    pub(crate) fn ret(&mut self, mut values: Vec<Value>) -> R<()> {
        if self.is_constructor && values.is_empty() {
            values = self.implicit_return()?;
        }
        self.terminate(Term::Return(values));
        Ok(())
    }

    // ---- blocks and instructions ----

    pub(crate) fn new_block(&mut self) -> Block {
        self.f.add_block()
    }

    /// Instructions go to `b` from now on.
    pub(crate) fn enter(&mut self, b: Block) {
        self.block = Some(b);
    }

    /// The current block; code after a `return` or `break` goes to a fresh block nothing
    /// reaches.
    pub(crate) fn current(&mut self) -> Block {
        match self.block {
            Some(b) => b,
            None => {
                let b = self.new_block();
                self.ssa.seal(&mut self.f, &mut self.l.module, b);
                self.block = Some(b);
                b
            }
        }
    }

    pub(crate) fn seal(&mut self, b: Block) {
        self.ssa.seal(&mut self.f, &mut self.l.module, b);
    }

    /// Ends the current block with `term`.
    pub(crate) fn terminate(&mut self, term: Term) {
        let b = self.current();
        for t in term.targets() {
            self.ssa.add_pred(t, b);
        }
        self.terms.insert(b, term);
        self.block = None;
    }

    /// Appends an instruction; its results are inferred (`explicit` for the ones that state
    /// their type).
    pub(crate) fn emit(&mut self, op: Op, operands: Vec<Value>, explicit: Option<Type>) -> R<Inst> {
        let b = self.current();
        let types: Vec<Type> = operands.iter().map(|&v| self.f.ty(v)).collect();
        let results = result_types(&mut self.l.module, &op, &types, explicit)
            .map_err(|e| LowerError::new("E1304", format!("lowering produced bad IR: {e}")))?;
        let inst = self.f.create_inst(op, operands, &results);
        self.f.insts[inst].loc = self.loc;
        self.f.append_inst(b, inst);
        Ok(inst)
    }

    /// [`emit`](Self::emit) for an instruction with one result.
    pub(crate) fn op(&mut self, op: Op, operands: Vec<Value>, explicit: Option<Type>) -> R<Value> {
        if let Some(v) = self.fold(&op, &operands, explicit)? {
            return Ok(v);
        }
        let inst = self.emit(op, operands, explicit)?;
        self.f.insts[inst]
            .results
            .first()
            .copied()
            .ok_or_else(|| LowerError::new("E1304", "an instruction without a result"))
    }

    /// A `function` variable of type `t`, made in the entry block.
    pub(crate) fn temp_var(&mut self, t: Type) -> R<Value> {
        let ptr = self.l.module.ptr(Space::Function, false, t);
        let inst = self.f.create_inst(Op::Var, Vec::new(), &[ptr]);
        match self.f.first_inst(self.entry) {
            Some(first) => self.f.insert_before(first, inst),
            None => self.f.append_inst(self.entry, inst),
        }
        Ok(self.f.result(inst))
    }

    // ---- variables ----

    pub(crate) fn read_var(&mut self, var: Var) -> Value {
        let b = self.current();
        self.ssa.read(&mut self.f, &mut self.l.module, var, b)
    }

    pub(crate) fn write_var(&mut self, var: Var, value: Value) {
        let b = self.current();
        self.ssa.write(var, b, value);
    }

    /// The variable of a local binding, made on first use.
    pub(crate) fn local(&mut self, b: BindingId) -> R<Local> {
        if let Some(&l) = self.locals.get(&b) {
            return Ok(l);
        }
        let ty = self.binding_ty(b);
        let t = self
            .l
            .ty(&ty)
            .map_err(|e| e.located(self.owner(), InferLoc::Binding(b)))?;
        let name = self.body.store.bindings[b].name.to_string();
        let var = self.ssa.new_var(t, Some(&name));
        let local = Local::Var(var);
        self.locals.insert(b, local);
        Ok(local)
    }

    // ---- finishing ----

    /// Seals every block, gives each jump its arguments, and makes the terminators.
    fn finish(&mut self) -> R<()> {
        let blocks: Vec<Block> = self.f.layout.clone();
        for &b in &blocks {
            self.seal(b);
        }
        for &b in &blocks {
            let term = self.terms.get(&b).cloned().unwrap_or(Term::Unreachable);
            let args = |cx: &Self, target: Block| -> Vec<Value> {
                cx.f.blocks[target]
                    .params
                    .iter()
                    .map(|p| cx.ssa.phi_args.get(p).and_then(|m| m.get(&b)).copied())
                    .map(|v| v.expect("every predecessor passes every parameter"))
                    .collect()
            };
            let (op, operands) = match term {
                Term::Br(t) => (Op::Br(t), args(self, t)),
                Term::CondBr {
                    cond,
                    then_block,
                    else_block,
                    merge,
                } => {
                    let (ta, ea) = (args(self, then_block), args(self, else_block));
                    let op = Op::CondBr {
                        then_block,
                        then_args: ta.len() as u32,
                        else_block,
                        merge,
                    };
                    (op, [vec![cond], ta, ea].concat())
                }
                Term::Switch {
                    value,
                    cases,
                    default,
                    merge,
                } => {
                    let mut operands = vec![value];
                    let mut ir_cases = Vec::new();
                    for (v, target) in cases {
                        let a = args(self, target);
                        ir_cases.push(SwitchCase {
                            value: v,
                            block: target,
                            args: a.len() as u32,
                        });
                        operands.extend(a);
                    }
                    let da = args(self, default);
                    let op = Op::Switch {
                        cases: ir_cases,
                        default,
                        default_args: da.len() as u32,
                        merge,
                    };
                    operands.extend(da);
                    (op, operands)
                }
                Term::Loop {
                    header,
                    continuing,
                    merge,
                } => (
                    Op::Loop {
                        header,
                        continuing,
                        merge,
                    },
                    args(self, header),
                ),
                Term::Return(values) => (Op::Return, values),
                Term::Unreachable => (Op::Unreachable, Vec::new()),
            };
            let inst = self.f.create_inst(op, operands, &[]);
            self.f.append_inst(b, inst);
        }
        Ok(())
    }
}

impl FnCx<'_, '_, '_> {
    /// An operation on constants computed now, the way the interpreter computes it; `None`
    /// when the operation is not a pure one or an operand is not a constant.
    fn fold(&mut self, op: &Op, operands: &[Value], explicit: Option<Type>) -> R<Option<Value>> {
        let pure = matches!(
            op,
            Op::Binary(_)
                | Op::Unary(_)
                | Op::Icmp(_)
                | Op::Fcmp(_)
                | Op::Select
                | Op::Convert(_)
                | Op::Math(_)
                | Op::Splat
                | Op::Extract(_)
                | Op::Insert(_)
                | Op::Shuffle(_)
                | Op::MatMul
                | Op::Transpose
                | Op::MatColumn(_)
                | Op::MatRow(_)
                | Op::MatExtract(..)
                | Op::Construct
                | Op::ExtractField(_)
                | Op::InsertField(_)
                | Op::ExtractElem(_)
                | Op::InsertElem(_)
        );
        if !pure || operands.is_empty() {
            return Ok(None);
        }
        let mut vals = Vec::new();
        for &v in operands {
            match self.f.values[v].def {
                marlin_shaderc_ir::ValueDef::Const(c) => vals.push(self.l.const_val(c)?),
                _ => return Ok(None),
            }
        }
        let types: Vec<Type> = operands.iter().map(|&v| self.f.ty(v)).collect();
        let Ok(results) = result_types(&mut self.l.module, op, &types, explicit) else {
            return Ok(None);
        };
        let [result] = results[..] else {
            return Ok(None);
        };
        let Ok(val) =
            marlin_shaderc_ir::evaluate_op(&self.l.module, op, &vals, &types, Some(result))
        else {
            return Ok(None);
        };
        let c = self.l.val_const(&val, result)?;
        Ok(Some(self.f.const_value(c, result)))
    }
}

/// The initializer of the local `b`.
pub(crate) fn local_initializer(body: &Body, b: BindingId) -> Option<ExprId> {
    body.store.stmts.iter().find_map(|(_, s)| match s {
        Stmt::Local { declarators, .. } => declarators
            .iter()
            .find(|(d, _)| *d == b)
            .and_then(|(_, init)| *init),
        _ => None,
    })
}
