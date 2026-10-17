//! Statements and patterns.

use marlin_front_hir_def::store::{BindingId, Expr, LambdaBody, Pat, PatId, Stmt, StmtId};
use marlin_lang_query::Cancelled;

use super::member::Member;
use super::{InferCtx, InferLoc};
use crate::builtins::Scalar;
use crate::ty::{ConstTy, Ty};

impl InferCtx<'_, '_> {
    pub(crate) fn check_stmt(&mut self, s: StmtId) -> Result<(), Cancelled> {
        let stmt = self.body.store.stmts[s].clone();
        match stmt {
            Stmt::Missing | Stmt::Break | Stmt::Continue | Stmt::Empty => {}
            Stmt::Expr(e) => {
                self.infer_expr(e, None)?;
            }
            Stmt::Local {
                ty, declarators, ..
            } => {
                let declared = match ty {
                    Some(t) => Some(self.lower_type(t)?),
                    None => None,
                };
                for (binding, init) in declarators {
                    let ty = match (&declared, init) {
                        (Some(t), Some(init)) => {
                            let t = t.clone();
                            self.check_expr(init, &t)?;
                            t
                        }
                        (Some(t), None) => t.clone(),
                        (None, Some(init)) => {
                            let t = self.infer_expr(init, None)?;
                            if t == Ty::Void {
                                self.report(InferLoc::Expr(init), "E1123", "this has no value");
                                Ty::Error
                            } else {
                                t
                            }
                        }
                        (None, None) => {
                            self.report(
                                InferLoc::Binding(binding),
                                "E1123",
                                "a `var` local needs an initializer",
                            );
                            Ty::Error
                        }
                    };
                    self.result.binding_types.insert(binding, ty);
                }
            }
            Stmt::Block(stmts) => {
                // Local functions can be called before their declaration in the block.
                for &st in &stmts {
                    if let Stmt::LocalFunction(f) = &self.body.store.stmts[st] {
                        let f = (**f).clone();
                        self.declare_local_function(&f)?;
                    }
                }
                for st in stmts {
                    self.check_stmt(st)?;
                }
            }
            Stmt::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                self.condition(cond)?;
                self.check_stmt(then)?;
                if let Some(o) = otherwise {
                    self.check_stmt(o)?;
                }
            }
            Stmt::For {
                init,
                cond,
                update,
                body,
            } => {
                for st in init {
                    self.check_stmt(st)?;
                }
                if let Some(c) = cond {
                    self.condition(c)?;
                }
                for u in update {
                    self.infer_expr(u, None)?;
                }
                self.check_stmt(body)?;
            }
            Stmt::Foreach {
                variable,
                iterable,
                body,
            } => {
                let element = self.iteration_element(iterable)?;
                self.infer_expr(variable, Some(&element))?;
                if let Expr::DeclVar { ty: Some(_), .. } = &self.body.store.exprs[variable] {
                    let declared = self.result.type_of(variable);
                    if !self.implicit(&element, &declared, super::Lit::None)? {
                        let (a, b) = (self.show(&element), self.show(&declared));
                        self.report(
                            InferLoc::Expr(variable),
                            "E1103",
                            format!("cannot convert `{a}` to `{b}` implicitly"),
                        );
                    }
                }
                self.check_stmt(body)?;
            }
            Stmt::While { cond, body } => {
                self.condition(cond)?;
                self.check_stmt(body)?;
            }
            Stmt::Do { body, cond } => {
                self.check_stmt(body)?;
                self.condition(cond)?;
            }
            Stmt::Switch {
                scrutinee,
                sections,
            } => {
                let ty = self.infer_expr(scrutinee, None)?;
                for section in sections {
                    for (pat, guard) in section.labels.iter().flatten() {
                        self.check_pat(*pat, &ty)?;
                        if let Some(g) = guard {
                            self.condition(*g)?;
                        }
                    }
                    for st in section.body {
                        self.check_stmt(st)?;
                    }
                }
            }
            Stmt::Return(value) => match (self.return_ty.clone(), value) {
                (Some(Ty::Void), Some(v)) => {
                    self.infer_expr(v, None)?;
                    self.report(InferLoc::Expr(v), "E1124", "this function returns nothing");
                }
                (Some(Ty::Void), None) | (None, _) => {}
                (Some(t), Some(v)) => self.check_expr(v, &t)?,
                (Some(t), None) => {
                    if !t.is_error() {
                        let shown = self.show(&t);
                        self.report(
                            InferLoc::Stmt(s),
                            "E1124",
                            format!("this function returns `{shown}`"),
                        );
                    }
                }
            },
            Stmt::LocalFunction(f) => {
                if self.local_function(f.binding).is_none() {
                    self.declare_local_function(&f)?;
                }
                let (params, ret) = self.local_function(f.binding).expect("declared");
                for (p, t) in f.params.iter().zip(params) {
                    self.result.binding_types.insert(p.binding, t);
                }
                let saved = self.return_ty.replace(ret.clone());
                match f.body {
                    Some(LambdaBody::Expr(e)) => {
                        if ret == Ty::Void {
                            self.infer_expr(e, None)?;
                        } else {
                            self.check_expr(e, &ret)?;
                        }
                    }
                    Some(LambdaBody::Block(b)) => self.check_stmt(b)?,
                    None => {}
                }
                self.return_ty = saved;
            }
        }
        Ok(())
    }

    fn declare_local_function(
        &mut self,
        f: &marlin_front_hir_def::store::LocalFunction,
    ) -> Result<(), Cancelled> {
        let mut params = Vec::new();
        for p in &f.params {
            params.push(match p.ty {
                Some(t) => self.lower_type(t)?,
                None => Ty::Error,
            });
        }
        let ret = self.lower_type(f.ret)?;
        self.local_functions.insert(f.binding, (params, ret));
        Ok(())
    }

    pub(crate) fn local_function(&self, binding: BindingId) -> Option<(Vec<Ty>, Ty)> {
        self.local_functions.get(&binding).cloned()
    }

    fn condition(&mut self, e: marlin_front_hir_def::store::ExprId) -> Result<(), Cancelled> {
        let b = self.bool_ty();
        self.check_expr(e, &b)
    }

    /// The element type `foreach` gets from `iterable`: integers for a range, elements for an
    /// array.
    fn iteration_element(
        &mut self,
        iterable: marlin_front_hir_def::store::ExprId,
    ) -> Result<Ty, Cancelled> {
        if let Expr::Range { start, end } = self.body.store.exprs[iterable].clone() {
            let mut ty: Option<Ty> = None;
            for x in [start, end].into_iter().flatten() {
                let t = self.infer_expr(x, None)?;
                match self.scalar_of(&t) {
                    Some(
                        Scalar::I32
                        | Scalar::U32
                        | Scalar::I16
                        | Scalar::U16
                        | Scalar::I64
                        | Scalar::U64,
                    ) => {
                        ty = Some(match ty {
                            Some(prev) if prev != t && self.literal_of(x) != super::Lit::None => {
                                prev
                            }
                            _ => t,
                        });
                    }
                    _ if t.is_error() => {}
                    _ => {
                        let shown = self.show(&t);
                        self.report(
                            InferLoc::Expr(x),
                            "E1114",
                            format!("a range bound is an integer, not `{shown}`"),
                        );
                    }
                }
            }
            let ty = ty.unwrap_or_else(|| self.scalar_ty(Scalar::I32));
            self.record(iterable, ty.clone());
            return Ok(ty);
        }
        let ty = self.infer_expr(iterable, None)?;
        Ok(match &ty {
            Ty::Array { element, .. } => (**element).clone(),
            Ty::Error => Ty::Error,
            _ => {
                let shown = self.show(&ty);
                self.report(
                    InferLoc::Expr(iterable),
                    "E1114",
                    format!("cannot `foreach` over `{shown}`"),
                );
                Ty::Error
            }
        })
    }

    /// Checks a pattern against the type of the value it matches.
    pub(crate) fn check_pat(&mut self, p: PatId, ty: &Ty) -> Result<(), Cancelled> {
        let pat = self.body.store.pats[p].clone();
        match pat {
            Pat::Missing | Pat::Discard => {}
            Pat::Constant(e) => self.check_expr(e, ty)?,
            Pat::Relational { value, .. } => {
                if !ty.is_error() && self.scalar_of(ty).is_none_or(|s| s == Scalar::Bool) {
                    let shown = self.show(ty);
                    self.report(
                        InferLoc::Pat(p),
                        "E1125",
                        format!("`{shown}` cannot be compared with `<` or `>`"),
                    );
                }
                self.check_expr(value, ty)?;
            }
            Pat::Var(b) => {
                self.result.binding_types.insert(b, ty.clone());
            }
            Pat::And(a, b) | Pat::Or(a, b) => {
                self.check_pat(a, ty)?;
                self.check_pat(b, ty)?;
            }
            Pat::Not(a) => self.check_pat(a, ty)?,
            Pat::Property(items) => {
                for (members, sub) in items {
                    let mut at = ty.clone();
                    for name in members.iter().flatten() {
                        if at.is_error() {
                            break;
                        }
                        at = match self.lookup_member(&at, name, false)? {
                            Member::Field { ty, .. }
                            | Member::Property { ty, .. }
                            | Member::Swizzle { ty, .. } => ty,
                            _ => {
                                let shown = self.show(&at);
                                self.report(
                                    InferLoc::Pat(sub),
                                    "E1111",
                                    format!("`{shown}` has no member `{name}`"),
                                );
                                Ty::Error
                            }
                        };
                    }
                    self.check_pat(sub, &at)?;
                }
            }
            Pat::Positional(items) => match ty {
                Ty::Tuple(elements) if elements.len() == items.len() => {
                    for (sub, t) in items.iter().zip(elements.iter()) {
                        self.check_pat(*sub, t)?;
                    }
                }
                Ty::Error => {
                    for sub in items {
                        self.check_pat(sub, &Ty::Error)?;
                    }
                }
                _ => {
                    let shown = self.show(ty);
                    self.report(
                        InferLoc::Pat(p),
                        "E1125",
                        format!("`{shown}` cannot be matched by position"),
                    );
                }
            },
        }
        let _ = ConstTy::Error;
        Ok(())
    }
}
