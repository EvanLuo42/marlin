//! Lambdas and local functions (0004 §9.2): each one is a function of its own whose first
//! parameter is its environment, a struct of the values it captures (by value, when the
//! lambda is written or the local function called). A function with `Func`/`Action`
//! parameters gets one instance per combination of lambdas passed to it; inside, calling the
//! parameter calls that lambda's function with the environment the parameter holds.

use std::collections::{BTreeSet, HashSet};

use marlin_front_hir_def::store::{BindingId, BindingKind, Expr, ExprId, Node, Pat, Stmt, StmtId};
use marlin_front_hir_def::{BodyQuery, ItemId, ItemKind, ItemTreeQuery, item_path};
use marlin_shaderc_hir_ty::{BuiltinKind, Callee, GenericArgTy, InferQuery, Root, Subst, Ty};
use marlin_shaderc_ir::{Op, Type, Value};

use crate::func::{FnCx, Local, signature};
use crate::{Instance, LowerError, Lowering, R};

/// Which lambda or local function of a body an instance is.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Inner {
    Lambda(ExprId),
    /// The `Stmt::LocalFunction`, and its result type (`None` for `void`).
    LocalFn {
        stmt: StmtId,
        ret: Option<Ty>,
    },
    /// A function passed by name (a method group), with its concrete type arguments. It has
    /// no function of its own: calling it calls the function, with `this` from the
    /// environment for an instance method.
    Method {
        item: ItemId,
        subst: Subst,
    },
}

/// One captured value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Capture {
    This,
    Binding(BindingId),
}

pub(crate) fn is_func(l: &Lowering<'_, '_>, ty: &Ty) -> bool {
    matches!(ty, Ty::Adt { item, .. } if matches!(l.builtins.kind(*item), Some(BuiltinKind::Func | BuiltinKind::Action)))
}

/// `Func<.., R>`'s `R`; `None` for an `Action`.
pub(crate) fn func_result(l: &Lowering<'_, '_>, ty: &Ty) -> Option<Ty> {
    match ty {
        Ty::Adt { item, args } if l.builtins.kind(*item) == Some(BuiltinKind::Func) => {
            match args.last() {
                Some(GenericArgTy::Type(t)) => Some(t.clone()),
                _ => None,
            }
        }
        _ => None,
    }
}

impl Lowering<'_, '_> {
    /// `lambda`, a local function's name, or the name of the function passed.
    pub(crate) fn inner_name(&self, instance: &Instance, inner: &Inner) -> R<String> {
        Ok(match inner {
            Inner::Lambda(_) => "lambda".to_string(),
            Inner::LocalFn { stmt, .. } => {
                let body = self.cx.get::<BodyQuery>(&instance.owner)?;
                match &body.store.stmts[*stmt] {
                    Stmt::LocalFunction(f) => body.store.bindings[f.binding].name.to_string(),
                    _ => "local".to_string(),
                }
            }
            Inner::Method { item, .. } => item_path(self.cx, *item)?.to_string(),
        })
    }

    /// The values `instance` (a lambda or local function) captures, `this` first.
    pub(crate) fn captures(&self, instance: &Instance) -> R<Vec<Capture>> {
        let root = match &instance.inner {
            None => return Ok(Vec::new()),
            Some(Inner::Lambda(e)) => Node::Expr(*e),
            Some(Inner::LocalFn { stmt, .. }) => Node::Stmt(*stmt),
            Some(Inner::Method { item, .. }) => {
                let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
                let data = &tree.items[item.local];
                let instance_method = data
                    .parent
                    .is_some_and(|p| tree.items[p].kind != ItemKind::Namespace)
                    && !data.is_static;
                return Ok(if instance_method {
                    vec![Capture::This]
                } else {
                    Vec::new()
                });
            }
        };
        let body = self.cx.get::<BodyQuery>(&instance.owner)?;
        let infer = self.cx.get::<InferQuery>(&instance.owner)?;
        let mut out = BTreeSet::new();
        let mut seen = HashSet::new();
        collect(&body, &infer, root, &mut out, &mut seen);
        Ok(out.into_iter().collect())
    }

    /// The struct of an inner instance's captured values.
    pub(crate) fn env_type(&mut self, instance: &Instance) -> R<Type> {
        if let Some(&t) = self.envs.get(instance) {
            return Ok(t);
        }
        let mut fields = Vec::new();
        for c in self.captures(instance)? {
            let (name, t) = match c {
                Capture::This => {
                    let outer = Instance {
                        inner: None,
                        ..instance.clone()
                    };
                    let sig = signature(self, &outer)?;
                    let ty = match (sig.this, sig.is_constructor) {
                        (Some((t, _)), _) => t,
                        (None, true) => sig.result.unwrap_or(Ty::Error),
                        _ => {
                            return Err(LowerError::new(
                                "E1305",
                                "a capture of `this` where there is none",
                            ));
                        }
                    };
                    ("this".to_string(), self.ty(&ty)?)
                }
                Capture::Binding(b) => {
                    let body = self.cx.get::<BodyQuery>(&instance.owner)?;
                    let infer = self.cx.get::<InferQuery>(&instance.owner)?;
                    let ty = self.resolve_assoc(
                        &instance.subst.ty(&infer
                            .binding_types
                            .get(b)
                            .cloned()
                            .unwrap_or(Ty::Error)),
                    );
                    let name = body.store.bindings[b].name.to_string();
                    let t = if is_func(self, &ty) {
                        let closure = bound_closure(&body, instance, b).ok_or_else(|| {
                            LowerError::new("E1301", "capturing this `Func` is not lowered yet")
                        })?;
                        self.env_type(&closure)?
                    } else {
                        self.ty(&ty)?
                    };
                    (name, t)
                }
            };
            fields.push((name.into(), t));
        }
        let base = format!("{}.closure", self.instance_base(instance)?);
        let mut name = base.clone();
        let mut k = 1;
        while self.module.structs.iter().any(|(_, s)| *s.name == *name) {
            name = format!("{base}#{k}");
            k += 1;
        }
        let id = self.module.add_struct(&name, fields);
        let t = self.module.struct_ty(id);
        self.envs.insert(instance.clone(), t);
        Ok(t)
    }
}

/// The lambda or local function a `Func` parameter binding of `instance`'s body is bound to.
pub(crate) fn bound_closure(
    body: &marlin_front_hir_def::Body,
    instance: &Instance,
    b: BindingId,
) -> Option<Instance> {
    let BindingKind::Param(i) = body.store.bindings[b].kind else {
        return None;
    };
    instance
        .closures
        .iter()
        .find(|(p, _)| *p == i)
        .map(|(_, c)| c.clone())
}

/// The local function statement that declares binding `b`.
pub(crate) fn local_function_stmt(
    body: &marlin_front_hir_def::Body,
    b: BindingId,
) -> Option<StmtId> {
    body.store.stmts.iter().find_map(|(s, stmt)| match stmt {
        Stmt::LocalFunction(f) if f.binding == b => Some(s),
        _ => None,
    })
}

/// Everything under `root` reads that is declared outside it.
fn collect(
    body: &marlin_front_hir_def::Body,
    infer: &marlin_shaderc_hir_ty::InferenceResult,
    root: Node,
    out: &mut BTreeSet<Capture>,
    seen: &mut HashSet<Node>,
) {
    if !seen.insert(root) {
        return;
    }
    let store = &body.store;
    let nodes = store.descendants(root);
    let mut declared = HashSet::new();
    for node in &nodes {
        match *node {
            Node::Expr(e) => match &store.exprs[e] {
                Expr::DeclVar { binding, .. } => {
                    declared.insert(*binding);
                }
                Expr::DeclTuple(bs) => declared.extend(bs.iter().copied()),
                Expr::Lambda { params, .. } => declared.extend(params.iter().map(|p| p.binding)),
                _ => {}
            },
            Node::Stmt(s) => match &store.stmts[s] {
                Stmt::Local { declarators, .. } => declared.extend(declarators.iter().map(|d| d.0)),
                Stmt::LocalFunction(f) => {
                    declared.insert(f.binding);
                    declared.extend(f.params.iter().map(|p| p.binding));
                }
                _ => {}
            },
            Node::Pat(p) => {
                if let Pat::Var(b) = store.pats[p] {
                    declared.insert(b);
                }
            }
        }
    }
    let mut found = BTreeSet::new();
    for node in &nodes {
        let Node::Expr(e) = *node else { continue };
        if let Expr::This = store.exprs[e] {
            found.insert(Capture::This);
        }
        if let Some(path) = infer.paths.get(e) {
            match path.root {
                Root::Local(b) if !declared.contains(&b) => {
                    found.insert(Capture::Binding(b));
                }
                Root::This => {
                    found.insert(Capture::This);
                }
                _ => {}
            }
        }
        if let Some(Callee::Invoke(b)) = infer.callees.get(e)
            && !declared.contains(b)
        {
            match store.bindings[*b].kind {
                // A local function called from here: what it captures, this captures too.
                BindingKind::LocalFunction => {
                    if let Some(s) = local_function_stmt(body, *b) {
                        collect(body, infer, Node::Stmt(s), &mut found, seen);
                    }
                }
                _ => {
                    found.insert(Capture::Binding(*b));
                }
            }
        }
    }
    out.extend(found.into_iter().filter(|c| match c {
        Capture::Binding(b) => !declared.contains(b),
        Capture::This => true,
    }));
}

impl FnCx<'_, '_, '_> {
    /// The environment value of `closure`: its captured values as they are now.
    pub(crate) fn make_env(&mut self, closure: &Instance) -> R<Value> {
        let t = self.l.env_type(closure)?;
        let mut parts = Vec::new();
        for c in self.l.captures(closure)? {
            parts.push(match c {
                Capture::This => match self.this.clone() {
                    Some((Local::Var(var), _)) => self.read_var(var),
                    Some((Local::Ptr(p), _)) => {
                        self.op(Op::Load { aligned: None }, vec![p], None)?
                    }
                    None => return Err(LowerError::new("E1305", "no `this` to capture")),
                },
                Capture::Binding(b) => match self.local(b)? {
                    Local::Var(var) => self.read_var(var),
                    Local::Ptr(p) => self.op(Op::Load { aligned: None }, vec![p], None)?,
                },
            });
        }
        self.op(Op::Construct, parts, Some(t))
    }

    /// The closure an argument for a `Func`/`Action` parameter passes, and its environment.
    pub(crate) fn closure_arg(&mut self, arg: ExprId) -> R<(Instance, Value)> {
        let loc = marlin_shaderc_hir_ty::InferLoc::Expr(arg);
        match self.body.store.exprs[arg].clone() {
            Expr::Lambda { .. } => {
                let closure = Instance {
                    inner: Some(Inner::Lambda(arg)),
                    ..self.instance.clone()
                };
                let env = self.make_env(&closure)?;
                Ok((closure, env))
            }
            Expr::Path(_) if self.infer.method_groups.get(arg).is_some() => {
                let (item, group) = self.infer.method_groups[arg].clone();
                let mut subst = self.subst().clone();
                for (owner, args) in self.concrete_subst(&group).entries {
                    subst.entries.retain(|(o, _)| *o != owner);
                    subst.entries.push((owner, args));
                }
                let closure = Instance {
                    inner: Some(Inner::Method { item, subst }),
                    ..self.instance.clone()
                };
                let env = self.make_env(&closure)?;
                Ok((closure, env))
            }
            Expr::Path(_) => {
                // A `Func` parameter passed on: the same lambda, the same environment.
                let scope = self.infer.paths.get(arg).map(|p| p.root.clone());
                let Some(Root::Local(b)) = scope else {
                    return Err(LowerError::unsupported(
                        "passing this as a `Func`",
                        self.owner(),
                        loc,
                    ));
                };
                let closure = bound_closure(&self.body, &self.instance, b).ok_or_else(|| {
                    LowerError::unsupported("passing this as a `Func`", self.owner(), loc.clone())
                })?;
                let env = match self.local(b)? {
                    Local::Var(var) => self.read_var(var),
                    Local::Ptr(p) => self.op(Op::Load { aligned: None }, vec![p], None)?,
                };
                Ok((closure, env))
            }
            _ => Err(LowerError::unsupported(
                "this argument for a `Func`",
                self.owner(),
                loc,
            )),
        }
    }
}

impl FnCx<'_, '_, '_> {
    /// `f(args)` for a `Func`/`Action` parameter or a local function `b`.
    pub(crate) fn invoke(
        &mut self,
        e: ExprId,
        b: BindingId,
        args: &[marlin_front_hir_def::store::Arg],
    ) -> R<Option<Value>> {
        let loc = marlin_shaderc_hir_ty::InferLoc::Expr(e);
        let (closure, env) = match self.body.store.bindings[b].kind {
            BindingKind::LocalFunction => {
                let stmt = local_function_stmt(&self.body, b).ok_or_else(|| {
                    self.error(
                        "E1305",
                        "a local function that is not declared",
                        loc.clone(),
                    )
                })?;
                let ty = self.expr_ty(e);
                let ret = (ty != Ty::Void).then_some(ty);
                let closure = Instance {
                    inner: Some(Inner::LocalFn { stmt, ret }),
                    ..self.instance.clone()
                };
                let env = self.make_env(&closure)?;
                (closure, env)
            }
            _ => {
                let closure = bound_closure(&self.body, &self.instance, b).ok_or_else(|| {
                    LowerError::unsupported("calling this `Func`", self.owner(), loc.clone())
                })?;
                let env = match self.local(b)? {
                    Local::Var(var) => self.read_var(var),
                    Local::Ptr(p) => self.op(Op::Load { aligned: None }, vec![p], None)?,
                };
                (closure, env)
            }
        };
        if let Some(Inner::Method { item, subst }) = &closure.inner {
            let receiver = if self.l.captures(&closure)?.is_empty() {
                None
            } else {
                let this = self.op(Op::ExtractField(0), vec![env], None)?;
                Some(crate::expr::Receiver::Value(this))
            };
            let mut values = Vec::new();
            for a in args {
                values.push(crate::expr::CallArg::Value(self.value(a.expr)?));
            }
            return self.call_function(e, *item, subst, receiver, values);
        }
        let sig =
            signature(self.l, &closure).map_err(|err| err.located(self.owner(), loc.clone()))?;
        let id = self
            .l
            .instance(closure)
            .map_err(|err| err.located(self.owner(), loc.clone()))?;
        let mut operands = vec![env];
        for (a, (_, pass)) in args.iter().zip(&sig.params) {
            if *pass != crate::func::Pass::Value {
                return Err(LowerError::unsupported(
                    "`ref` and `out` lambda parameters",
                    self.owner(),
                    loc,
                ));
            }
            operands.push(self.value(a.expr)?);
        }
        let inst = self.emit(Op::Call(id), operands, None)?;
        Ok(self.f.insts[inst].results.first().copied())
    }
}
