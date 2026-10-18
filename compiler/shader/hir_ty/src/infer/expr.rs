//! Expressions.

use marlin_front_hir_def::store::{
    BinaryOp, Expr, ExprId, FieldInit, HirPath, InterpolationPart, LambdaBody, Literal,
};
use marlin_front_hir_def::{ItemKind, ItemTreeQuery, Resolution};
use marlin_lang_query::Cancelled;

use super::call::ArgInfo;
use super::conv::natural_int;
use super::member::{Member, Step};
use super::{Callee, InferCtx, InferLoc, Lit, Root, ValuePath};
use crate::builtins::{BuiltinKind, Scalar};
use crate::item::ItemTypesQuery;
use crate::subst::Subst;
use crate::ty::{ConstTy, GenericArgTy, Ty};

impl InferCtx<'_, '_> {
    /// Infers `e`, using `expected` where an expression takes its type from context.
    pub(crate) fn infer_expr(&mut self, e: ExprId, expected: Option<&Ty>) -> Result<Ty, Cancelled> {
        let ty = self.infer_inner(e, expected)?;
        Ok(self.record(e, ty))
    }

    /// Infers `e` against `expected` and converts it there.
    pub(crate) fn check_expr(&mut self, e: ExprId, expected: &Ty) -> Result<(), Cancelled> {
        let ty = self.infer_expr(e, Some(expected))?;
        self.coerce(e, &ty, expected)
    }

    /// Converts expression `e` of type `from` to `to` implicitly, or reports why not.
    pub(crate) fn coerce(&mut self, e: ExprId, from: &Ty, to: &Ty) -> Result<(), Cancelled> {
        if from == to || from.is_error() || to.is_error() {
            return Ok(());
        }
        if self.implicit(from, to, self.literal_of(e))? {
            self.result.coercions.insert(e, to.clone());
            return Ok(());
        }
        let (f, t) = (self.show(from), self.show(to));
        let hint = if self.explicit(from, to)? {
            format!("; write `({t})` to convert explicitly")
        } else {
            String::new()
        };
        self.report(
            InferLoc::Expr(e),
            "E1103",
            format!("cannot convert `{f}` to `{t}` implicitly{hint}"),
        );
        Ok(())
    }

    fn infer_inner(&mut self, e: ExprId, expected: Option<&Ty>) -> Result<Ty, Cancelled> {
        let expr = self.body.store.exprs[e].clone();
        Ok(match expr {
            Expr::Missing => Ty::Error,
            Expr::Literal(lit) => self.literal(e, &lit, expected),
            Expr::Interpolated(parts) => {
                for part in parts {
                    if let InterpolationPart::Hole {
                        expr, alignment, ..
                    } = part
                    {
                        self.infer_expr(expr, None)?;
                        if let Some(a) = alignment {
                            let int = self.scalar_ty(Scalar::I32);
                            self.check_expr(a, &int)?;
                        }
                    }
                }
                self.builtins
                    .item(BuiltinKind::String)
                    .map_or(Ty::Error, Ty::adt)
            }
            Expr::Path(path) => match expected {
                Some(target) if self.func_signature(target).is_some() => {
                    match self.method_group_items(e)? {
                        Some(items) => {
                            let target = target.clone();
                            self.method_group(e, &items, &target)?
                        }
                        None => self.path_value(e, &path)?,
                    }
                }
                _ => self.path_value(e, &path)?,
            },
            Expr::Type(_) => {
                self.report(InferLoc::Expr(e), "E1115", "a type is not a value here");
                Ty::Error
            }
            Expr::Member { receiver, name, .. } => {
                let recv = self.infer_expr(receiver, None)?;
                if recv.is_error() {
                    return Ok(Ty::Error);
                }
                let member = self.lookup_member(&recv, &name, false)?;
                let (ty, step) = self.member_value(e, &recv, &name, member)?;
                if let Some(step) = &step {
                    self.result.member_steps.insert(e, step.clone());
                }
                if let Some(step) = step {
                    // Extend the receiver's path, if it has one.
                    let mut path = self
                        .result
                        .paths
                        .get(receiver)
                        .cloned()
                        .unwrap_or(ValuePath {
                            root: Root::This,
                            steps: Vec::new(),
                        });
                    if self.result.paths.get(receiver).is_some() {
                        path.steps.push(step);
                        self.result.paths.insert(e, path);
                    }
                }
                ty
            }
            Expr::Call { callee, args } => self.call(e, callee, &args)?,
            Expr::Index { base, args } => self.index(e, base, &args)?,
            Expr::Unary { op, operand } => self.unary(e, op, operand)?,
            Expr::Binary { op, lhs, rhs } => {
                if matches!(op, BinaryOp::And | BinaryOp::Or) {
                    let b = self.bool_ty();
                    self.check_expr(lhs, &b)?;
                    self.check_expr(rhs, &b)?;
                    self.result.callees.insert(e, Callee::BuiltinOp);
                    b
                } else {
                    self.binary(e, op, lhs, rhs)?
                }
            }
            Expr::Assign { op, target, value } => self.assign(e, op, target, value)?,
            Expr::Conditional {
                cond,
                then,
                otherwise,
            } => {
                let b = self.bool_ty();
                self.check_expr(cond, &b)?;
                let a = self.infer_expr(then, expected)?;
                let c = self.infer_expr(otherwise, expected)?;
                match expected {
                    Some(t) => {
                        self.coerce(then, &a, t)?;
                        self.coerce(otherwise, &c, t)?;
                        t.clone()
                    }
                    None => self.common_type(e, (then, &a), (otherwise, &c))?,
                }
            }
            Expr::Cast { ty, expr } => {
                let target = self.lower_type(ty)?;
                let source = self.infer_expr(expr, None)?;
                if !self.explicit(&source, &target)? && !source.is_error() && !target.is_error() {
                    let (s, t) = (self.show(&source), self.show(&target));
                    self.report(
                        InferLoc::Expr(e),
                        "E1108",
                        format!("cannot convert `{s}` to `{t}`"),
                    );
                }
                target
            }
            Expr::Range { start, end } => {
                for x in [start, end].into_iter().flatten() {
                    self.infer_expr(x, None)?;
                }
                self.report(
                    InferLoc::Expr(e),
                    "E1109",
                    "a range is only allowed as the collection of a `foreach`",
                );
                Ty::Error
            }
            Expr::Is { expr, pat } => {
                let ty = self.infer_expr(expr, None)?;
                self.check_pat(pat, &ty)?;
                self.bool_ty()
            }
            Expr::Switch { scrutinee, arms } => {
                let ty = self.infer_expr(scrutinee, None)?;
                let mut arm_tys = Vec::new();
                for arm in &arms {
                    self.check_pat(arm.pat, &ty)?;
                    if let Some(guard) = arm.guard {
                        let b = self.bool_ty();
                        self.check_expr(guard, &b)?;
                    }
                    arm_tys.push((arm.expr, self.infer_expr(arm.expr, expected)?));
                }
                let result = match expected {
                    Some(t) => t.clone(),
                    None => {
                        // The first arm's type that every other arm converts to.
                        let mut chosen = None;
                        for (_, candidate) in &arm_tys {
                            let mut all = true;
                            for (x, t) in &arm_tys {
                                if !self.implicit(t, candidate, self.literal_of(*x))? {
                                    all = false;
                                    break;
                                }
                            }
                            if all {
                                chosen = Some(candidate.clone());
                                break;
                            }
                        }
                        match chosen {
                            Some(t) => t,
                            None => {
                                self.report(
                                    InferLoc::Expr(e),
                                    "E1116",
                                    "the arms of this switch have no common type",
                                );
                                Ty::Error
                            }
                        }
                    }
                };
                for (x, t) in arm_tys {
                    self.coerce(x, &t, &result)?;
                }
                result
            }
            Expr::With { expr, inits } => {
                let ty = self.infer_expr(expr, None)?;
                self.field_inits(&ty, &inits)?;
                ty
            }
            Expr::ObjectInit { ty, inits } => {
                let target = match ty {
                    Some(t) => self.lower_type(t)?,
                    None => match expected {
                        Some(t) => t.clone(),
                        None => {
                            self.report(
                                InferLoc::Expr(e),
                                "E1117",
                                "the type of `{ ... }` cannot be inferred here; write `T { ... }`",
                            );
                            Ty::Error
                        }
                    },
                };
                self.field_inits(&target, &inits)?;
                if let Ty::Adt { .. } = &target {
                    self.result
                        .callees
                        .insert(e, Callee::DefaultConstruct(target.clone()));
                }
                target
            }
            Expr::Default(ty) => match ty {
                Some(t) => self.lower_type(t)?,
                None => match expected {
                    Some(t) => t.clone(),
                    None => {
                        self.report(
                            InferLoc::Expr(e),
                            "E1117",
                            "the type of `default` cannot be inferred here; write `default(T)`",
                        );
                        Ty::Error
                    }
                },
            },
            Expr::This => match &self.this_ty {
                Some(t) => t.clone(),
                None => {
                    self.report(InferLoc::Expr(e), "E1118", "there is no `this` here");
                    Ty::Error
                }
            },
            // Rejected by the shader dialect already.
            Expr::Base
            | Expr::New { .. }
            | Expr::Typeof(_)
            | Expr::Sizeof(_)
            | Expr::Throw(_)
            | Expr::As { .. } => Ty::Error,
            Expr::Collection(items) => self.collection(e, &items, expected)?,
            Expr::Tuple(items) => {
                let expected_items = match expected {
                    Some(Ty::Tuple(t)) if t.len() == items.len() => Some(t.clone()),
                    _ => None,
                };
                let mut tys = Vec::new();
                for (i, (_, x)) in items.iter().enumerate() {
                    let ex = expected_items.as_ref().map(|t| t[i].clone());
                    let t = self.infer_expr(*x, ex.as_ref())?;
                    tys.push(t);
                }
                Ty::Tuple(tys.into())
            }
            Expr::Lambda { .. } => match expected {
                Some(target) if self.func_signature(target).is_some() => {
                    let target = target.clone();
                    self.infer_lambda(e, &target, false)?;
                    target
                }
                _ => {
                    self.report(
                        InferLoc::Expr(e),
                        "E1110",
                        "a lambda can only be passed to a `Func` or `Action` parameter",
                    );
                    Ty::Error
                }
            },
            Expr::DeclVar { ty, binding } => {
                let ty = match ty {
                    Some(t) => self.lower_type(t)?,
                    None => expected.cloned().unwrap_or(Ty::Error),
                };
                self.result.binding_types.insert(binding, ty.clone());
                ty
            }
            Expr::DeclTuple(bindings) => match expected {
                Some(Ty::Tuple(items)) if items.len() == bindings.len() => {
                    for (b, t) in bindings.iter().zip(items.iter()) {
                        self.result.binding_types.insert(*b, t.clone());
                    }
                    Ty::Tuple(items.clone())
                }
                _ => {
                    for &b in &bindings {
                        self.result.binding_types.insert(b, Ty::Error);
                    }
                    if expected.is_some_and(|t| !t.is_error()) {
                        let shown = self.show(expected.unwrap());
                        self.report(
                            InferLoc::Expr(e),
                            "E1119",
                            format!(
                                "cannot deconstruct `{shown}` into {} variables",
                                bindings.len()
                            ),
                        );
                    }
                    Ty::Error
                }
            },
        })
    }

    fn literal(&mut self, e: ExprId, lit: &Literal, expected: Option<&Ty>) -> Ty {
        match lit {
            Literal::Bool(_) => self.bool_ty(),
            Literal::Null | Literal::Char(_) => Ty::Error,
            Literal::String(_) => self
                .builtins
                .item(BuiltinKind::String)
                .map_or(Ty::Error, Ty::adt),
            Literal::Number(text) => {
                let flexible = self.literal_of(e);
                // An unsuffixed literal takes a numeric scalar type from context.
                if flexible != Lit::None
                    && let Some(target) = expected
                    && let Some(s) = self.scalar_of(target)
                    && self
                        .implicit(&self.scalar_ty(natural_scalar(text)), target, flexible)
                        .unwrap_or(false)
                    && s != Scalar::Bool
                {
                    return target.clone();
                }
                self.scalar_ty(natural_scalar(text))
            }
        }
    }

    /// The type both branches of `?:` convert to.
    fn common_type(
        &mut self,
        e: ExprId,
        (a, at): (ExprId, &Ty),
        (b, bt): (ExprId, &Ty),
    ) -> Result<Ty, Cancelled> {
        if at == bt {
            return Ok(at.clone());
        }
        if self.implicit(at, bt, self.literal_of(a))? {
            self.coerce(a, at, bt)?;
            return Ok(bt.clone());
        }
        if self.implicit(bt, at, self.literal_of(b))? {
            self.coerce(b, bt, at)?;
            return Ok(at.clone());
        }
        let (x, y) = (self.show(at), self.show(bt));
        self.report(
            InferLoc::Expr(e),
            "E1116",
            format!("`{x}` and `{y}` have no common type"),
        );
        Ok(Ty::Error)
    }

    // ---- paths ----

    /// The value a path names: a local, a field or constant, `this`'s members, a chain of
    /// members after any of those.
    fn path_value(&mut self, e: ExprId, path: &HirPath) -> Result<Ty, Cancelled> {
        let def_path = path.to_def_path();
        let scope = self.scopes.scope_of_expr(e).unwrap_or_else(|| {
            self.scopes
                .scopes
                .iter()
                .next()
                .map(|(id, _)| id)
                .expect("root scope")
        });
        if self.used_before_declaration(e, scope, path) {
            return Ok(Ty::Error);
        }
        let resolution =
            self.resolver
                .resolve_in_body(self.cx, &def_path, true, &self.scopes, scope)?;
        // A constant generic parameter is a value of its type (`N * 2`, `0..N`).
        if let Resolution::TypeParam { owner, index } = &resolution
            && def_path.segments.len() == 1
        {
            let types = self.cx.get::<ItemTypesQuery>(owner)?;
            if let Some(ty) = types
                .generics
                .get(*index as usize)
                .and_then(|g| g.const_ty.clone())
            {
                self.result.paths.insert(
                    e,
                    ValuePath {
                        root: Root::ConstParam {
                            owner: *owner,
                            index: *index,
                        },
                        steps: vec![],
                    },
                );
                return Ok(ty);
            }
        }
        let (mut ty, mut value, mut statics, start) = match resolution {
            Resolution::Local(b) => {
                let ty = self.binding_ty(b);
                (
                    ty,
                    Some(ValuePath {
                        root: Root::Local(b),
                        steps: vec![],
                    }),
                    false,
                    def_path.segments.len(),
                )
            }
            Resolution::Values(items) => return self.values(e, &items, path),
            Resolution::Partial { base, resolved } => match *base {
                Resolution::Local(b) => {
                    let ty = self.binding_ty(b);
                    (
                        ty,
                        Some(ValuePath {
                            root: Root::Local(b),
                            steps: vec![],
                        }),
                        false,
                        resolved,
                    )
                }
                Resolution::Type(item) => (Ty::adt(item), None, true, resolved),
                Resolution::TypeParam { owner, index } => {
                    (Ty::Param { owner, index }, None, true, resolved)
                }
                _ => unreachable!("partial resolutions start at a local or a type"),
            },
            other => {
                self.unresolved_value(e, &def_path.to_string(), other)?;
                return Ok(Ty::Error);
            }
        };
        for segment in &def_path.segments[start..] {
            if ty.is_error() {
                return Ok(Ty::Error);
            }
            let member = self.lookup_member(&ty, &segment.name, statics)?;
            let (next, step) = self.member_value(e, &ty, &segment.name, member)?;
            if statics {
                // `Type.Member`: the first static step starts the value.
                if let Some(Step::Field(item) | Step::Property(item)) = &step {
                    value = Some(ValuePath {
                        root: Root::Item(*item),
                        steps: vec![],
                    });
                    statics = false;
                    ty = next;
                    continue;
                }
                statics = matches!(next, Ty::Adt { .. }) && step.is_none();
            }
            if let (Some(v), Some(step)) = (&mut value, step) {
                v.steps.push(step);
            }
            ty = next;
        }
        if statics && !ty.is_error() {
            self.report(
                InferLoc::Expr(e),
                "E1115",
                format!("`{def_path}` is a type, not a value"),
            );
            return Ok(Ty::Error);
        }
        if let Some(v) = value {
            self.result.paths.insert(e, v);
        }
        Ok(ty)
    }

    /// C#'s rule that a block's locals cover the whole block: a name a later local of an
    /// enclosing block declares is that local, used before its declaration.
    fn used_before_declaration(
        &mut self,
        e: ExprId,
        scope: marlin_front_hir_def::ScopeId,
        path: &HirPath,
    ) -> bool {
        let first = match path.segments.first() {
            Some(first) if !path.global && first.args.is_none() => first,
            _ => return false,
        };
        if self.scopes.declared_later(scope, &first.name).is_none() {
            return false;
        }
        self.report(
            InferLoc::Expr(e),
            "E1128",
            format!("`{}` is used before its declaration", first.name),
        );
        true
    }

    pub(crate) fn binding_ty(&self, b: marlin_front_hir_def::store::BindingId) -> Ty {
        self.result
            .binding_types
            .get(b)
            .cloned()
            .unwrap_or(Ty::Error)
    }

    /// A field or constant named directly (`Taps`, or `State` through the implicit `this`).
    fn values(
        &mut self,
        e: ExprId,
        items: &[marlin_front_hir_def::ItemId],
        path: &HirPath,
    ) -> Result<Ty, Cancelled> {
        let item = items[0];
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        let data = tree.items[item.local].clone();
        match data.kind {
            ItemKind::Field | ItemKind::EnumMember | ItemKind::Property => {
                let types = self.cx.get::<ItemTypesQuery>(&item)?;
                let ty = self.in_enum_initializer(item, types.ty.clone().unwrap_or(Ty::Error))?;
                let instance = data
                    .parent
                    .is_some_and(|p| tree.items[p].kind != ItemKind::Namespace)
                    && !data.is_static
                    && !data.is_const
                    && data.kind != ItemKind::EnumMember;
                let step = if data.kind == ItemKind::Property {
                    Step::Property(item)
                } else {
                    Step::Field(item)
                };
                let value = if instance {
                    if self.this_ty.is_none() {
                        self.report(
                            InferLoc::Expr(e),
                            "E1118",
                            format!(
                                "`{}` is an instance member; there is no `this` here",
                                path.to_def_path()
                            ),
                        );
                    }
                    ValuePath {
                        root: Root::This,
                        steps: vec![step],
                    }
                } else {
                    ValuePath {
                        root: Root::Item(item),
                        steps: vec![],
                    }
                };
                self.result.paths.insert(e, value);
                Ok(ty)
            }
            _ => {
                self.report(
                    InferLoc::Expr(e),
                    "E1115",
                    format!("`{}` is a function; call it", path.to_def_path()),
                );
                Ok(Ty::Error)
            }
        }
    }

    fn unresolved_value(
        &mut self,
        e: ExprId,
        text: &str,
        resolution: Resolution,
    ) -> Result<(), Cancelled> {
        let (code, message) = match resolution {
            Resolution::Type(_) | Resolution::TypeParam { .. } => {
                ("E1115", format!("`{text}` is a type, not a value"))
            }
            Resolution::Namespace(_) => ("E1115", format!("`{text}` is a namespace, not a value")),
            Resolution::Inaccessible { .. } => {
                ("E1112", format!("`{text}` is not accessible here"))
            }
            Resolution::Ambiguous { .. } => ("E1113", format!("`{text}` is ambiguous")),
            _ => ("E1111", format!("cannot find `{text}` in this scope")),
        };
        self.report(InferLoc::Expr(e), code, message);
        Ok(())
    }

    /// The value of a member found by lookup, and the path step it adds.
    fn member_value(
        &mut self,
        e: ExprId,
        recv: &Ty,
        name: &marlin_front_hir_def::Name,
        member: Member,
    ) -> Result<(Ty, Option<Step>), Cancelled> {
        Ok(match member {
            Member::Field { item, ty } => {
                let ty = self.in_enum_initializer(item, ty)?;
                (ty, Some(Step::Field(item)))
            }
            Member::Property { item, ty } => (ty, Some(Step::Property(item))),
            Member::Swizzle { components, ty } => (ty, Some(Step::Swizzle(components))),
            Member::Length => (self.scalar_ty(Scalar::I32), Some(Step::Length)),
            Member::Type(ty) => (ty, None),
            Member::Methods(_) => {
                self.report(
                    InferLoc::Expr(e),
                    "E1115",
                    format!("`{name}` is a method; call it"),
                );
                (Ty::Error, None)
            }
            Member::Inaccessible => {
                self.report(
                    InferLoc::Expr(e),
                    "E1112",
                    format!("`{name}` is not accessible here"),
                );
                (Ty::Error, None)
            }
            Member::None => {
                let shown = self.show(recv);
                self.report(
                    InferLoc::Expr(e),
                    "E1111",
                    format!("`{shown}` has no member `{name}`"),
                );
                (Ty::Error, None)
            }
        })
    }

    // ---- calls ----

    fn call(
        &mut self,
        e: ExprId,
        callee: ExprId,
        args: &[marlin_front_hir_def::store::Arg],
    ) -> Result<Ty, Cancelled> {
        let callee_expr = self.body.store.exprs[callee].clone();
        match callee_expr {
            Expr::Path(path) => {
                let def_path = path.to_def_path();
                let scope = self.scopes.scope_of_expr(callee).unwrap_or_else(|| {
                    self.scopes
                        .scopes
                        .iter()
                        .next()
                        .map(|(id, _)| id)
                        .expect("root scope")
                });
                if self.used_before_declaration(callee, scope, &path) {
                    self.arg_infos(args)?;
                    return Ok(Ty::Error);
                }
                let explicit = self.explicit_args(&path)?;
                let resolution =
                    self.resolver
                        .resolve_in_body(self.cx, &def_path, true, &self.scopes, scope)?;
                match resolution {
                    Resolution::Values(items) => {
                        let functions: Vec<_> = items.iter().map(|&i| (i, Subst::new())).collect();
                        let infos = self.arg_infos(args)?;
                        let name = format!("`{def_path}`");
                        let ty =
                            self.resolve_call(e, &functions, explicit.as_deref(), &infos, &name)?;
                        Ok(ty)
                    }
                    Resolution::Type(_) => {
                        let ty = self.type_of_path(callee, &path)?;
                        self.construct(e, &ty, args)
                    }
                    Resolution::Local(b) => self.invoke(e, b, args),
                    Resolution::Partial { .. } => {
                        // Everything but the last segment is the receiver.
                        let (prefix, last) =
                            def_path.segments.split_at(def_path.segments.len() - 1);
                        let last = &last[0];
                        let mut receiver_path = path.clone();
                        receiver_path.segments.truncate(prefix.len());
                        let receiver_resolution = self.resolver.resolve_in_body(
                            self.cx,
                            &receiver_path.to_def_path(),
                            true,
                            &self.scopes,
                            scope,
                        )?;
                        let (recv_ty, statics) = match receiver_resolution {
                            Resolution::Type(item) => (
                                self.type_of_path(callee, &receiver_path)?.or(Ty::adt(item)),
                                true,
                            ),
                            Resolution::TypeParam { owner, index } => {
                                (Ty::Param { owner, index }, true)
                            }
                            _ => (self.path_value(callee, &receiver_path)?, false),
                        };
                        if recv_ty.is_error() {
                            self.arg_infos(args)?;
                            return Ok(Ty::Error);
                        }
                        self.method_call(
                            e,
                            &recv_ty,
                            &last.name,
                            statics,
                            explicit.as_deref(),
                            args,
                        )
                    }
                    other => {
                        self.unresolved_value(callee, &def_path.to_string(), other)?;
                        self.arg_infos(args)?;
                        Ok(Ty::Error)
                    }
                }
            }
            Expr::Member { receiver, name, .. } => {
                let recv = self.infer_expr(receiver, None)?;
                if recv.is_error() {
                    self.arg_infos(args)?;
                    return Ok(Ty::Error);
                }
                let explicit = match &self.body.store.exprs[callee] {
                    Expr::Member { args: Some(a), .. } => {
                        Some(self.lower_generic_args(&a.clone())?)
                    }
                    _ => None,
                };
                self.method_call(e, &recv, &name, false, explicit.as_deref(), args)
            }
            Expr::Type(t) => {
                let ty = self.lower_type(t)?;
                self.record(callee, ty.clone());
                self.construct(e, &ty, args)
            }
            _ => {
                self.infer_expr(callee, None)?;
                self.arg_infos(args)?;
                self.report(InferLoc::Expr(callee), "E1120", "this cannot be called");
                Ok(Ty::Error)
            }
        }
    }

    fn method_call(
        &mut self,
        e: ExprId,
        recv: &Ty,
        name: &marlin_front_hir_def::Name,
        statics: bool,
        explicit: Option<&[GenericArgTy]>,
        args: &[marlin_front_hir_def::store::Arg],
    ) -> Result<Ty, Cancelled> {
        self.result.receivers.insert(e, recv.clone());
        match self.lookup_member(recv, name, statics)? {
            Member::Methods(methods) => {
                let infos = self.arg_infos(args)?;
                self.resolve_call(e, &methods, explicit, &infos, &format!("`{name}`"))
            }
            other => {
                let (ty, _) = self.member_value(e, recv, name, other)?;
                self.arg_infos(args)?;
                if !ty.is_error() {
                    self.report(
                        InferLoc::Expr(e),
                        "E1120",
                        format!("`{name}` is not a method"),
                    );
                }
                Ok(Ty::Error)
            }
        }
    }

    /// Explicit type arguments on the last segment: `F<float>(x)`.
    fn explicit_args(&mut self, path: &HirPath) -> Result<Option<Vec<GenericArgTy>>, Cancelled> {
        match path.segments.last().and_then(|s| s.args.clone()) {
            Some(args) => Ok(Some(self.lower_generic_args(&args)?)),
            None => Ok(None),
        }
    }

    fn lower_generic_args(
        &mut self,
        args: &[marlin_front_hir_def::store::GenericArg],
    ) -> Result<Vec<GenericArgTy>, Cancelled> {
        let mut out = Vec::new();
        for a in args {
            out.push(match a {
                marlin_front_hir_def::store::GenericArg::Type(t) => {
                    GenericArgTy::Type(self.lower_type(*t)?)
                }
                marlin_front_hir_def::store::GenericArg::Const(x) => {
                    let mut lower = self.type_lowerer();
                    let c = lower.const_expr(*x)?;
                    GenericArgTy::Const(c)
                }
            });
        }
        Ok(out)
    }

    fn type_lowerer(&self) -> crate::lower::TyLowerer<'_, '_> {
        crate::lower::TyLowerer {
            cx: self.cx,
            owner: self.owner.item,
            store: &self.body.store,
            resolver: self.resolver.clone(),
            builtins: self.builtins.clone(),
            diagnostics: Vec::new(),
            in_attribute: false,
        }
    }

    /// The type a path in an expression names (`float3`, `Light`, `Vector<float, 2>`).
    fn type_of_path(&mut self, e: ExprId, path: &HirPath) -> Result<Ty, Cancelled> {
        let mut lower = self.type_lowerer();
        let ty = lower.path_type(path, crate::TyLoc::Expr(e))?;
        let diagnostics = std::mem::take(&mut lower.diagnostics);
        for d in diagnostics {
            self.forward_diagnostic(d);
        }
        Ok(ty)
    }

    pub(crate) fn forward_diagnostic(&mut self, d: crate::TyDiagnostic) {
        let loc = match d.loc {
            crate::TyLoc::Type(t) => InferLoc::Type(t),
            crate::TyLoc::Expr(e) => InferLoc::Expr(e),
            crate::TyLoc::Item => InferLoc::Owner,
        };
        let (code, message) = (d.code().0, d.message());
        self.report(loc, code, message);
    }

    /// `T(args)`: a declared constructor, `T()` for a struct without one, or a built-in value.
    fn construct(
        &mut self,
        e: ExprId,
        ty: &Ty,
        args: &[marlin_front_hir_def::store::Arg],
    ) -> Result<Ty, Cancelled> {
        let Ty::Adt {
            item,
            args: type_args,
        } = ty
        else {
            self.arg_infos(args)?;
            return Ok(Ty::Error);
        };
        if let Some(element) = self.vector_of(ty) {
            return self.construct_vector(e, ty, element, args);
        }
        if self.scalar_of(ty).is_some() {
            // `float(x)`: the conversions that are implicit anyway.
            let infos = self.arg_infos(args)?;
            match infos.as_slice() {
                [one] => {
                    let from = one.ty.clone().unwrap_or(Ty::Error);
                    self.coerce(one.expr, &from, ty)?;
                }
                _ => self.report(
                    InferLoc::Expr(e),
                    "E1104",
                    format!("`{}` takes one argument", self.show(ty)),
                ),
            }
            self.result
                .callees
                .insert(e, Callee::BuiltinConstruct(ty.clone()));
            return Ok(ty.clone());
        }
        if self.builtins.kind(*item).is_some() {
            self.arg_infos(args)?;
            self.report(
                InferLoc::Expr(e),
                "E1120",
                format!("`{}` cannot be constructed this way", self.show(ty)),
            );
            return Ok(Ty::Error);
        }
        let constructors = self.constructors(*item, type_args)?;
        if constructors.is_empty() {
            self.arg_infos(args)?;
            if !args.is_empty() {
                self.report(
                    InferLoc::Expr(e),
                    "E1104",
                    format!(
                        "`{}` declares no constructor; write `{} {{ Field = value }}`",
                        self.show(ty),
                        self.show(ty)
                    ),
                );
            }
            self.result
                .callees
                .insert(e, Callee::DefaultConstruct(ty.clone()));
            return Ok(ty.clone());
        }
        let infos = self.arg_infos(args)?;
        let shown = format!("`{}`", self.show(ty));
        self.resolve_call(e, &constructors, None, &infos, &shown)?;
        Ok(ty.clone())
    }

    /// `float3(1, 2, 3)`, `float4(v.xyz, 1)`, `float3(0.5)`: scalars and vectors whose
    /// components add up to the vector's length (one scalar is broadcast).
    fn construct_vector(
        &mut self,
        e: ExprId,
        ty: &Ty,
        (element, n): (Ty, ConstTy),
        args: &[marlin_front_hir_def::store::Arg],
    ) -> Result<Ty, Cancelled> {
        let infos = self.arg_infos(args)?;
        let mut count: i128 = 0;
        for info in &infos {
            let from = info.ty.clone().unwrap_or(Ty::Error);
            if let Some((fe, fnn)) = self.vector_of(&from) {
                count += fnn.value().unwrap_or(0);
                let target = self.vector_ty(element.clone(), fnn);
                if !self.implicit(&fe, &element, Lit::None)? {
                    self.coerce(info.expr, &from, &target)?;
                } else if from != target {
                    self.result.coercions.insert(info.expr, target);
                }
            } else {
                count += 1;
                self.coerce(info.expr, &from, &element)?;
            }
        }
        let broadcast = infos.len() == 1 && count == 1;
        if let Some(len) = n.value()
            && count != len
            && !broadcast
        {
            self.report(
                InferLoc::Expr(e),
                "E1104",
                format!("`{}` needs {len} components, {count} given", self.show(ty)),
            );
        }
        self.result
            .callees
            .insert(e, Callee::BuiltinConstruct(ty.clone()));
        Ok(ty.clone())
    }

    /// Calling a `Func`/`Action` parameter.
    fn invoke(
        &mut self,
        e: ExprId,
        binding: marlin_front_hir_def::store::BindingId,
        args: &[marlin_front_hir_def::store::Arg],
    ) -> Result<Ty, Cancelled> {
        let ty = self.binding_ty(binding);
        if let Some(sig) = self.local_function(binding) {
            let infos = self.arg_infos(args)?;
            return self.check_simple_call(e, &sig.0, &sig.1, &infos, Callee::Invoke(binding));
        }
        let Some((params, ret)) = self.func_signature(&ty) else {
            self.arg_infos(args)?;
            if !ty.is_error() {
                let shown = self.show(&ty);
                self.report(
                    InferLoc::Expr(e),
                    "E1120",
                    format!("a value of type `{shown}` cannot be called"),
                );
            }
            return Ok(Ty::Error);
        };
        let infos = self.arg_infos(args)?;
        self.check_simple_call(e, &params, &ret, &infos, Callee::Invoke(binding))
    }

    fn check_simple_call(
        &mut self,
        e: ExprId,
        params: &[Ty],
        ret: &Ty,
        infos: &[ArgInfo],
        callee: Callee,
    ) -> Result<Ty, Cancelled> {
        if params.len() != infos.len() {
            self.report(
                InferLoc::Expr(e),
                "E1104",
                format!(
                    "{} argument(s) expected, {} given",
                    params.len(),
                    infos.len()
                ),
            );
            return Ok(Ty::Error);
        }
        for (info, param) in infos.iter().zip(params) {
            self.check_arg(info, param)?;
        }
        self.result.callees.insert(e, callee);
        Ok(ret.clone())
    }

    // ---- indexing, assignment, initializers ----

    fn index(
        &mut self,
        e: ExprId,
        base: ExprId,
        args: &[marlin_front_hir_def::store::Arg],
    ) -> Result<Ty, Cancelled> {
        let ty = self.infer_expr(base, None)?;
        if ty.is_error() {
            self.arg_infos(args)?;
            return Ok(Ty::Error);
        }
        let index_ty = |s: &mut Self, x: ExprId| -> Result<(), Cancelled> {
            let t = s.infer_expr(x, None)?;
            match s.scalar_of(&t) {
                Some(Scalar::I32 | Scalar::U32 | Scalar::I16 | Scalar::U16) => Ok(()),
                _ if t.is_error() => Ok(()),
                _ => {
                    let shown = s.show(&t);
                    s.report(
                        InferLoc::Expr(x),
                        "E1121",
                        format!("an index is `int` or `uint`, not `{shown}`"),
                    );
                    Ok(())
                }
            }
        };
        if let Ty::Array { element, .. } = &ty
            && let [one] = args
        {
            index_ty(self, one.expr)?;
            return Ok((**element).clone());
        }
        if let Some((element, _)) = self.vector_of(&ty)
            && let [one] = args
        {
            index_ty(self, one.expr)?;
            return Ok(element);
        }
        if let Some((element, _, _)) = self.matrix_of(&ty)
            && let [r, c] = args
        {
            index_ty(self, r.expr)?;
            index_ty(self, c.expr)?;
            return Ok(element);
        }
        // Declared indexers (resources, user types).
        if let Ty::Adt {
            item,
            args: type_args,
        } = &ty
        {
            let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
            let subst = Subst::new().with_args(*item, type_args);
            let indexers: Vec<_> = tree.items[item.local]
                .members
                .iter()
                .filter(|&&m| tree.items[m].kind == ItemKind::Indexer)
                .map(|&m| {
                    (
                        marlin_front_hir_def::ItemId {
                            file: item.file,
                            local: m,
                        },
                        subst.clone(),
                    )
                })
                .collect();
            if !indexers.is_empty() {
                let infos = self.arg_infos(args)?;
                return self.resolve_call(e, &indexers, None, &infos, "the indexer");
            }
        }
        self.arg_infos(args)?;
        let shown = self.show(&ty);
        self.report(
            InferLoc::Expr(e),
            "E1121",
            format!("`{shown}` cannot be indexed like this"),
        );
        Ok(Ty::Error)
    }

    fn assign(
        &mut self,
        e: ExprId,
        op: Option<BinaryOp>,
        target: ExprId,
        value: ExprId,
    ) -> Result<Ty, Cancelled> {
        // `var (a, b) = t;` and `(a, b) = (b, a);`
        if let Expr::DeclTuple(_) = &self.body.store.exprs[target] {
            let vt = self.infer_expr(value, None)?;
            self.infer_expr(target, Some(&vt))?;
            return Ok(Ty::Void);
        }
        let tt = self.infer_expr(target, None)?;
        self.check_place(target)?;
        match op {
            None => self.check_expr(value, &tt)?,
            Some(op) => {
                let rt = self.binary(e, op, target, value)?;
                if !rt.is_error() {
                    self.coerce(e, &rt, &tt)?;
                }
            }
        }
        Ok(tt)
    }

    /// Reports `e` if it is not something that can be assigned.
    pub(crate) fn check_place(&mut self, e: ExprId) -> Result<(), Cancelled> {
        let ok = match &self.body.store.exprs[e] {
            Expr::Path(_) | Expr::Member { .. } => match self.result.paths.get(e) {
                Some(path) => {
                    if let Some(Step::Swizzle(c)) = path.steps.last() {
                        let mut seen = c.clone();
                        seen.sort();
                        seen.dedup();
                        if seen.len() != c.len() {
                            self.report(
                                InferLoc::Expr(e),
                                "E1107",
                                "a swizzle that is assigned to cannot repeat a component",
                            );
                        }
                    }
                    true
                }
                None => self.result.expr_types.get(e).is_none_or(Ty::is_error),
            },
            Expr::Index { .. } | Expr::DeclVar { .. } | Expr::DeclTuple(_) => true,
            Expr::Tuple(items) => {
                let items = items.clone();
                for (_, x) in items {
                    self.check_place(x)?;
                }
                true
            }
            _ => false,
        };
        if !ok {
            self.report(InferLoc::Expr(e), "E1107", "this cannot be assigned to");
        }
        Ok(())
    }

    fn field_inits(&mut self, ty: &Ty, inits: &[FieldInit]) -> Result<(), Cancelled> {
        for init in inits {
            if ty.is_error() {
                self.infer_expr(init.value, None)?;
                continue;
            }
            match self.lookup_member(ty, &init.name, false)? {
                Member::Field { ty: ft, .. } | Member::Property { ty: ft, .. } => {
                    self.check_expr(init.value, &ft)?;
                }
                _ => {
                    let shown = self.show(ty);
                    self.report(
                        InferLoc::Expr(init.value),
                        "E1111",
                        format!("`{shown}` has no field `{}`", init.name),
                    );
                    self.infer_expr(init.value, None)?;
                }
            }
        }
        Ok(())
    }

    fn collection(
        &mut self,
        e: ExprId,
        items: &[ExprId],
        expected: Option<&Ty>,
    ) -> Result<Ty, Cancelled> {
        let element = match expected {
            Some(Ty::Array { element, len }) => {
                if let Some(n) = len.value()
                    && n != items.len() as i128
                {
                    self.report(
                        InferLoc::Expr(e),
                        "E1122",
                        format!("{} element(s) given for an array of {n}", items.len()),
                    );
                }
                Some((**element).clone())
            }
            _ => None,
        };
        let element = match element {
            Some(t) => {
                for &x in items {
                    self.check_expr(x, &t)?;
                }
                t
            }
            None => {
                let mut first = None;
                for &x in items {
                    match &first {
                        None => first = Some(self.infer_expr(x, None)?),
                        Some(t) => {
                            let t = t.clone();
                            self.check_expr(x, &t)?;
                        }
                    }
                }
                first.unwrap_or(Ty::Error)
            }
        };
        Ok(Ty::Array {
            element: element.into(),
            len: ConstTy::Value(items.len() as i128),
        })
    }

    /// Types a lambda against a `Func`/`Action` target; returns its parameter and result types.
    pub(crate) fn infer_lambda(
        &mut self,
        e: ExprId,
        target: &Ty,
        _speculative: bool,
    ) -> Result<Option<(Vec<Ty>, Ty)>, Cancelled> {
        let Expr::Lambda { params, body } = self.body.store.exprs[e].clone() else {
            return Ok(None);
        };
        let Some((param_tys, ret)) = self.func_signature(target) else {
            return Ok(None);
        };
        if params.len() != param_tys.len() {
            self.report(
                InferLoc::Expr(e),
                "E1110",
                format!(
                    "this lambda takes {} parameter(s); the target takes {}",
                    params.len(),
                    param_tys.len()
                ),
            );
            return Ok(None);
        }
        for (p, t) in params.iter().zip(&param_tys) {
            let ty = match p.ty {
                Some(written) => self.lower_type(written)?,
                None => t.clone(),
            };
            self.result.binding_types.insert(p.binding, ty);
        }
        let saved = self.return_ty.take();
        let known_ret = !matches!(ret, Ty::Param { .. }) && !ret.is_error();
        let actual = match body {
            LambdaBody::Expr(x) => {
                if known_ret && ret != Ty::Void {
                    self.check_expr(x, &ret)?;
                    ret.clone()
                } else {
                    self.infer_expr(x, None)?
                }
            }
            LambdaBody::Block(s) => {
                self.return_ty = Some(if known_ret { ret.clone() } else { Ty::Error });
                self.check_stmt(s)?;
                ret.clone()
            }
        };
        self.return_ty = saved;
        self.record(e, target.clone());
        Ok(Some((param_tys, actual)))
    }
}

/// The natural type of a numeric literal (C#, with `float` for unsuffixed reals and `h` for
/// `half`).
fn natural_scalar(text: &str) -> Scalar {
    let lower = text.to_ascii_lowercase();
    let radix = lower.starts_with("0x") || lower.starts_with("0b");
    if !radix {
        if lower.ends_with('h') {
            return Scalar::F16;
        }
        if lower.ends_with('d') {
            return Scalar::F64;
        }
        if lower.ends_with('f') || lower.ends_with('m') || lower.contains(['.', 'e']) {
            return Scalar::F32;
        }
    }
    let value = crate::lower::parse_int(text).unwrap_or(0);
    if lower.ends_with("ul") || lower.ends_with("lu") {
        Scalar::U64
    } else if lower.ends_with('u') {
        if value > i128::from(u32::MAX) {
            Scalar::U64
        } else {
            Scalar::U32
        }
    } else if lower.ends_with('l') {
        if value > i128::from(i64::MAX) {
            Scalar::U64
        } else {
            Scalar::I64
        }
    } else {
        natural_int(value)
    }
}

trait OrTy {
    fn or(self, other: Ty) -> Ty;
}

impl OrTy for Ty {
    fn or(self, other: Ty) -> Ty {
        if self.is_error() { other } else { self }
    }
}
