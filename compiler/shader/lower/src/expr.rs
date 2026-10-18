//! Expressions: values, places (what assignments write), conversions, operators, calls,
//! constructors, indexing. C#'s order of evaluation: left to right, receiver before arguments.

use marlin_front_hir_def::store::{
    Arg, BinaryOp, BindingId, BindingKind, Expr, ExprId, FieldInit, InterpolationPart, Literal,
    RefKind, UnaryOp,
};
use marlin_front_hir_def::{BodyOwner, BodyPart, ItemKind, ItemSignature, ItemTreeQuery};
use marlin_shaderc_hir_ty::{
    BuiltinKind, Callee, ConstTy, GenericArgTy, InferLoc, ItemTypesQuery, Root, Step, Subst, Ty,
    ValuePath, parse_int,
};
use marlin_shaderc_ir::{
    AccessStep, BinaryOp as IrBin, ConstValue, ConvertOp, FloatCmp, IntCmp, Op, Type, TypeData,
    UnaryOp as IrUn, Value, ValueDef,
};

use crate::closures::Inner;
use crate::dispatch::Resolved;
use crate::func::{FnCx, Local, Pass, Term, signature};
use crate::types::{Num, Shape};
use crate::{Instance, LowerError, R};

impl FnCx<'_, '_, '_> {
    // ---- types of expressions ----

    /// The type `e` has before its implicit conversion.
    pub(crate) fn expr_ty(&self, e: ExprId) -> Ty {
        self.concrete(&self.infer.type_of(e))
    }

    /// The type `e` has after its implicit conversion.
    pub(crate) fn final_ty(&self, e: ExprId) -> Ty {
        match self.infer.coercions.get(e) {
            Some(t) => self.concrete(t),
            None => self.expr_ty(e),
        }
    }

    fn loc(e: ExprId) -> InferLoc {
        InferLoc::Expr(e)
    }

    // ---- constants ----

    pub(crate) fn konst(&mut self, ty: Type, value: ConstValue) -> Value {
        let c = self.l.module.constant(ty, value);
        self.f.const_value(c, ty)
    }

    pub(crate) fn int_const(&mut self, ty: Type, v: i64) -> Value {
        let c = self.l.module.int(ty, v);
        self.f.const_value(c, ty)
    }

    pub(crate) fn bool_const(&mut self, v: bool) -> Value {
        let c = self.l.module.bool(v);
        let ty = self.l.module.consts[c].ty;
        self.f.const_value(c, ty)
    }

    /// The bits of `v` if it is an integer constant.
    pub(crate) fn const_bits(&self, v: Value) -> Option<u64> {
        match self.f.values[v].def {
            ValueDef::Const(c) => match self.l.module.consts[c].value {
                ConstValue::Int(b) => Some(b),
                _ => None,
            },
            _ => None,
        }
    }

    /// One of `ty`'s numbers: 1 for the increments, of the right kind.
    fn one_of(&mut self, ty: &Ty) -> R<Value> {
        let t = self.l.ty(ty)?;
        let num = self.l.num(ty)?.unwrap_or(Num::Signed(32));
        if num.is_float() {
            let c = self.l.module.float(t, 1.0);
            Ok(self.f.const_value(c, t))
        } else {
            Ok(self.int_const(t, 1))
        }
    }

    /// A number literal as a constant of `ty` (its type after any implicit conversion).
    fn number(&mut self, text: &str, ty: &Ty, e: ExprId) -> R<Value> {
        let t = self
            .l
            .ty(ty)
            .map_err(|err| err.located(self.owner(), Self::loc(e)))?;
        let num = self.l.num(ty)?;
        match num {
            Some(n) if n.is_float() => {
                let clean: String = text
                    .chars()
                    .filter(|c| *c != '_')
                    .collect::<String>()
                    .trim_end_matches(['f', 'F', 'd', 'D', 'h', 'H', 'm', 'M'])
                    .to_string();
                let v: f64 = match parse_int(&clean) {
                    Some(i) => i as f64,
                    None => clean.parse().map_err(|_| {
                        self.error("E1305", "a number that does not parse", Self::loc(e))
                    })?,
                };
                // Vectors of a literal (after broadcast) are not literals' types; use the
                // element.
                let c = self.l.module.float(t, v);
                Ok(self.f.const_value(c, t))
            }
            Some(_) => {
                let v = parse_int(text).ok_or_else(|| {
                    self.error("E1305", "an integer that does not parse", Self::loc(e))
                })?;
                Ok(self.int_const(t, v as i64))
            }
            None => Err(self.error(
                "E1305",
                "a number of a type that is not numeric",
                Self::loc(e),
            )),
        }
    }

    // ---- expressions ----

    /// The value of `e` after its implicit conversion; `None` for `void` calls.
    pub(crate) fn expr(&mut self, e: ExprId) -> R<Option<Value>> {
        // A literal becomes a constant of the type it converts to, directly.
        if let Expr::Literal(Literal::Number(text)) = &self.body.store.exprs[e] {
            let text = text.clone();
            let ty = self.final_ty(e);
            if self
                .l
                .num_shape(&ty)?
                .is_some_and(|(_, s)| s == Shape::Scalar)
            {
                return self.number(&text, &ty, e).map(Some);
            }
        }
        if let Expr::Unary {
            op: UnaryOp::Neg,
            operand,
        } = &self.body.store.exprs[e]
            && let Expr::Literal(Literal::Number(text)) = &self.body.store.exprs[*operand]
            && self.infer.callees.get(e) == Some(&Callee::BuiltinOp)
        {
            let text = format!("-{text}");
            let ty = self.final_ty(e);
            if self
                .l
                .num_shape(&ty)?
                .is_some_and(|(_, s)| s == Shape::Scalar)
            {
                return self.number(&text, &ty, e).map(Some);
            }
        }
        // What this expression emits comes from its source.
        let outer = self.loc;
        self.loc = self.l.source_loc(self.owner(), &Self::loc(e))?.or(outer);
        let result =
            self.expr_raw(e)
                .and_then(|raw| match (raw, self.infer.coercions.get(e).cloned()) {
                    (Some(v), Some(to)) => {
                        let (from, to) = (self.expr_ty(e), self.concrete(&to));
                        self.convert(v, &from, &to, e).map(Some)
                    }
                    (raw, _) => Ok(raw),
                });
        self.loc = outer;
        result
    }

    /// The value of `e`, which must have one.
    pub(crate) fn value(&mut self, e: ExprId) -> R<Value> {
        self.expr(e)?
            .ok_or_else(|| self.error("E1305", "this has no value", Self::loc(e)))
    }

    fn expr_raw(&mut self, e: ExprId) -> R<Option<Value>> {
        let expr = self.body.store.exprs[e].clone();
        let unsupported =
            |s: &Self, what: &str| Err(LowerError::unsupported(what, s.owner(), Self::loc(e)));
        Ok(Some(match expr {
            Expr::Missing => return Err(self.error("E1305", "broken code", Self::loc(e))),
            Expr::Literal(Literal::Number(text)) => {
                let ty = self.expr_ty(e);
                self.number(&text, &ty, e)?
            }
            Expr::Literal(Literal::Bool(b)) => self.bool_const(b),
            Expr::Literal(_) | Expr::Interpolated(_) => {
                return unsupported(self, "a string or character");
            }
            Expr::Path(_) | Expr::Member { .. } | Expr::Index { .. } if self.is_mem_place(e)? => {
                let (p, _) = self.mem_place(e)?;
                self.op(Op::Load { aligned: None }, vec![p], None)?
            }
            Expr::Path(_) => {
                let path =
                    self.infer.paths.get(e).cloned().ok_or_else(|| {
                        self.error("E1305", "a name without a value", Self::loc(e))
                    })?;
                self.read_path(&path, e)?
            }
            Expr::Member { receiver, .. } => {
                if let Some(path) = self.infer.paths.get(e).cloned() {
                    self.read_path(&path, e)?
                } else {
                    let v = self.value(receiver)?;
                    let ty = self.final_ty(receiver);
                    let step = self.infer.member_steps.get(e).cloned().ok_or_else(|| {
                        self.error("E1305", "a member without a value", Self::loc(e))
                    })?;
                    self.apply_step(v, &ty, &step, e)?.0
                }
            }
            Expr::Call { callee, args } => return self.call(e, callee, &args),
            Expr::Index { base, args } => self.index_read(e, base, &args)?,
            Expr::Unary { op, operand } => self.unary(e, op, operand)?,
            Expr::Binary {
                op: op @ (BinaryOp::And | BinaryOp::Or),
                lhs,
                rhs,
            } => self.short_circuit(op, lhs, rhs)?,
            Expr::Binary { op, lhs, rhs } => self.binary(e, op, lhs, rhs)?,
            Expr::Assign { op, target, value } => self.assign(e, op, target, value)?,
            Expr::Conditional {
                cond,
                then,
                otherwise,
            } => self.conditional(e, cond, then, otherwise)?,
            Expr::Cast { expr, .. } => {
                let v = self.value(expr)?;
                let (from, to) = (self.final_ty(expr), self.expr_ty(e));
                self.convert(v, &from, &to, e)?
            }
            Expr::Is { expr, pat } => {
                let v = self.value(expr)?;
                let ty = self.final_ty(expr);
                self.pat_test(pat, v, &ty)?
            }
            Expr::Switch { scrutinee, arms } => self.switch_expr(e, scrutinee, &arms)?,
            Expr::With { expr, inits } => {
                let v = self.value(expr)?;
                let ty = self.expr_ty(e);
                self.field_inits(v, &ty, &inits, e)?
            }
            Expr::ObjectInit { inits, .. } => {
                let ty = self.expr_ty(e);
                let v = self.default_value(&ty, Self::loc(e))?;
                self.field_inits(v, &ty, &inits, e)?
            }
            Expr::Default(_) => {
                let t = self.l.ty(&self.expr_ty(e))?;
                self.konst(t, ConstValue::Zero)
            }
            Expr::This => match self.this.clone() {
                Some((Local::Var(var), _)) => self.read_var(var),
                Some((Local::Ptr(p), _)) => self.op(Op::Load { aligned: None }, vec![p], None)?,
                None => return Err(self.error("E1305", "no `this` here", Self::loc(e))),
            },
            Expr::Collection(items) => {
                let ty = self.final_ty(e);
                let t = self.l.ty(&ty)?;
                let mut parts = Vec::new();
                for x in items {
                    parts.push(self.value(x)?);
                }
                self.op(Op::Construct, parts, Some(t))?
            }
            Expr::Tuple(items) => {
                let ty = self.expr_ty(e);
                let t = self.l.ty(&ty)?;
                let mut parts = Vec::new();
                for (_, x) in items {
                    parts.push(self.value(x)?);
                }
                self.op(Op::Construct, parts, Some(t))?
            }
            Expr::Range { .. } => return unsupported(self, "a range outside `foreach`"),
            Expr::Lambda { .. } => return unsupported(self, "a lambda"),
            Expr::DeclVar { .. } | Expr::DeclTuple(_) => {
                return unsupported(self, "a declaration here");
            }
            Expr::As { .. }
            | Expr::Type(_)
            | Expr::Base
            | Expr::New { .. }
            | Expr::Typeof(_)
            | Expr::Sizeof(_)
            | Expr::Throw(_) => return unsupported(self, "this expression"),
        }))
    }

    // ---- paths: reading and writing ----

    fn root_value(&mut self, root: &Root, e: ExprId) -> R<(Value, Ty)> {
        Ok(match root {
            Root::ConstParam { owner, index } => {
                let types = self.l.cx.get::<ItemTypesQuery>(owner)?;
                let ty = types
                    .generics
                    .get(*index as usize)
                    .and_then(|g| g.const_ty.clone())
                    .unwrap_or(Ty::Error);
                let value = self.subst().konst(&ConstTy::Param {
                    owner: *owner,
                    index: *index,
                });
                let ConstTy::Value(v) = value else {
                    return Err(self.error(
                        "E1307",
                        "a constant generic parameter without a value here",
                        Self::loc(e),
                    ));
                };
                let t = self.l.ty(&ty)?;
                (self.int_const(t, v as i64), ty)
            }
            Root::Local(b) if self.is_const_local(*b) => {
                let v = self
                    .const_local(*b)
                    .map_err(|err| err.located(self.owner(), Self::loc(e)))?;
                (v, self.binding_ty(*b))
            }
            Root::Local(b) if matches!(self.instance.inner, Some(Inner::Const(_))) => {
                let name = &self.body.store.bindings[*b].name;
                return Err(self.error(
                    "E1306",
                    format!("a `const` initializer reads `{name}`, which is not a constant"),
                    Self::loc(e),
                ));
            }
            Root::Local(b) => {
                let ty = self.binding_ty(*b);
                let v = match self.local(*b)? {
                    Local::Var(var) => self.read_var(var),
                    Local::Ptr(p) => self.op(Op::Load { aligned: None }, vec![p], None)?,
                };
                (v, ty)
            }
            Root::This => match self.this.clone() {
                Some((Local::Var(var), ty)) => (self.read_var(var), ty),
                Some((Local::Ptr(p), ty)) => {
                    (self.op(Op::Load { aligned: None }, vec![p], None)?, ty)
                }
                None => return Err(self.error("E1305", "no `this` here", Self::loc(e))),
            },
            Root::Item(item) if self.l.shared_global(*item)?.is_some() => {
                let (p, ty) = self.shared_root(*item, e)?;
                (self.op(Op::Load { aligned: None }, vec![p], None)?, ty)
            }
            Root::Item(item) => {
                let tree = self.l.cx.get::<ItemTreeQuery>(&item.file)?;
                let data = &tree.items[item.local];
                if !(data.is_const || data.kind == ItemKind::EnumMember) {
                    return Err(LowerError::unsupported(
                        "a module-level variable",
                        self.owner(),
                        Self::loc(e),
                    ));
                }
                let types = self.l.cx.get::<ItemTypesQuery>(item)?;
                let ty = self.concrete(types.ty.as_ref().unwrap_or(&Ty::Error));
                let v = self
                    .l
                    .const_operand(*item, &ty)
                    .map_err(|err| err.located(self.owner(), Self::loc(e)))?;
                let v = self.f.const_value(v, self.l.module.consts[v].ty);
                (v, ty)
            }
        })
    }

    pub(crate) fn read_path(&mut self, path: &ValuePath, e: ExprId) -> R<Value> {
        let (mut v, mut ty) = self.root_value(&path.root, e)?;
        for step in &path.steps {
            (v, ty) = self.apply_step(v, &ty, step, e)?;
        }
        Ok(v)
    }

    /// A vector type of `element` with `n` components (the scalar itself for 1).
    fn vector_ty(&self, element: &Ty, n: usize) -> Ty {
        if n == 1 {
            return element.clone();
        }
        match self.l.builtins.item(BuiltinKind::Vector) {
            Some(item) => Ty::Adt {
                item,
                args: vec![
                    GenericArgTy::Type(element.clone()),
                    GenericArgTy::Const(ConstTy::Value(n as i128)),
                ]
                .into(),
            },
            None => Ty::Error,
        }
    }

    fn vector_element(ty: &Ty) -> Ty {
        match ty {
            Ty::Adt { args, .. } => match args.first() {
                Some(GenericArgTy::Type(t)) => t.clone(),
                _ => Ty::Error,
            },
            _ => Ty::Error,
        }
    }

    pub(crate) fn field_ty(&mut self, owner_ty: &Ty, field: marlin_front_hir_def::ItemId) -> R<Ty> {
        let types = self.l.cx.get::<ItemTypesQuery>(&field)?;
        let ty = types.ty.clone().unwrap_or(Ty::Error);
        Ok(match owner_ty {
            Ty::Adt { item, args } => Subst::new().with_args(*item, args).ty(&ty),
            _ => ty,
        })
    }

    /// Reads one step of a path from `v` (of type `ty`).
    pub(crate) fn apply_step(
        &mut self,
        v: Value,
        ty: &Ty,
        step: &Step,
        e: ExprId,
    ) -> R<(Value, Ty)> {
        Ok(match step {
            Step::Field(f) => {
                let i = self.l.field_index(ty, *f)?;
                let fty = self.field_ty(ty, *f)?;
                (self.op(Op::ExtractField(i), vec![v], None)?, fty)
            }
            Step::Swizzle(lanes) => {
                let element = Self::vector_element(ty);
                let out = self.vector_ty(&element, lanes.len());
                let v = if lanes.len() == 1 {
                    self.op(Op::Extract(lanes[0] as u32), vec![v], None)?
                } else {
                    let lanes = lanes.iter().map(|&l| l as u32).collect();
                    self.op(Op::Shuffle(lanes), vec![v, v], None)?
                };
                (v, out)
            }
            Step::Length => {
                let len = match self.l.ty(ty).map(|t| self.l.module.data(t).clone())? {
                    TypeData::Array(_, n) => n,
                    _ => {
                        return Err(self.error(
                            "E1305",
                            "`Length` of something that is not an array",
                            Self::loc(e),
                        ));
                    }
                };
                let i32_ty = self.l.ty(&self.expr_ty(e))?;
                (self.int_const(i32_ty, len as i64), self.expr_ty(e))
            }
            Step::Property(p) => self.property_get(*p, v, ty, e)?,
        })
    }

    /// `old` with the part at `steps` replaced by `value`.
    fn rebuild(
        &mut self,
        old: Value,
        ty: &Ty,
        steps: &[Step],
        value: Value,
        e: ExprId,
    ) -> R<Value> {
        let Some((step, rest)) = steps.split_first() else {
            return Ok(value);
        };
        match step {
            Step::Field(f) => {
                let i = self.l.field_index(ty, *f)?;
                let fty = self.field_ty(ty, *f)?;
                let inner = self.op(Op::ExtractField(i), vec![old], None)?;
                let new_inner = self.rebuild(inner, &fty, rest, value, e)?;
                self.op(Op::InsertField(i), vec![old, new_inner], None)
            }
            Step::Swizzle(lanes) if rest.is_empty() => {
                if lanes.len() == 1 {
                    return self.op(Op::Insert(lanes[0] as u32), vec![old, value], None);
                }
                let n = match self.l.module.data(self.f.ty(old)) {
                    TypeData::Vector(_, n) => *n as u32,
                    _ => {
                        return Err(self.error(
                            "E1305",
                            "a swizzle of something that is not a vector",
                            Self::loc(e),
                        ));
                    }
                };
                let picks: Vec<u32> = (0..n)
                    .map(|i| match lanes.iter().position(|&l| l as u32 == i) {
                        Some(k) => n + k as u32,
                        None => i,
                    })
                    .collect();
                // The written value has as many lanes as the swizzle; pad it to the vector's
                // width for the shuffle.
                let padded = self.pad_vector(value, n)?;
                self.op(Op::Shuffle(picks), vec![old, padded], None)
            }
            Step::Property(p) if rest.is_empty() => self.property_set(*p, old, ty, value, e),
            Step::Property(p) => {
                // `a.P.x = v`: get, change the part, set.
                let (inner, pty) = self.property_get(*p, old, ty, e)?;
                let new_inner = self.rebuild(inner, &pty, rest, value, e)?;
                self.property_set(*p, old, ty, new_inner, e)
            }
            _ => Err(LowerError::unsupported(
                "this assignment",
                self.owner(),
                Self::loc(e),
            )),
        }
    }

    /// `v` (a vector of `k <= n` lanes) widened to `n` lanes, repeating its first lane.
    fn pad_vector(&mut self, v: Value, n: u32) -> R<Value> {
        let k = match self.l.module.data(self.f.ty(v)) {
            TypeData::Vector(_, k) => *k as u32,
            _ => {
                return Err(LowerError::new(
                    "E1304",
                    "padding something that is not a vector",
                ));
            }
        };
        if k == n {
            return Ok(v);
        }
        let lanes: Vec<u32> = (0..n).map(|i| if i < k { i } else { 0 }).collect();
        self.op(Op::Shuffle(lanes), vec![v, v], None)
    }

    /// Writes `value` to the place `target` (a path, an element, a declaration or a tuple).
    pub(crate) fn assign_place(&mut self, target: ExprId, value: Value) -> R<()> {
        if self.is_mem_place(target)? {
            let (p, _) = self.mem_place(target)?;
            self.emit(Op::Store { aligned: None }, vec![p, value], None)?;
            return Ok(());
        }
        match self.body.store.exprs[target].clone() {
            Expr::DeclVar { binding, .. } => self.write_local(binding, value),
            Expr::DeclTuple(bindings) => {
                for (i, b) in bindings.into_iter().enumerate() {
                    let part = self.op(Op::ExtractField(i as u32), vec![value], None)?;
                    self.write_local(b, part)?;
                }
                Ok(())
            }
            Expr::Tuple(items) => {
                for (i, (_, x)) in items.into_iter().enumerate() {
                    let part = self.op(Op::ExtractField(i as u32), vec![value], None)?;
                    self.assign_place(x, part)?;
                }
                Ok(())
            }
            Expr::Index { base, args } => self.index_write(target, base, &args, value),
            _ => {
                let path = self.infer.paths.get(target).cloned().ok_or_else(|| {
                    self.error("E1305", "this cannot be assigned", Self::loc(target))
                })?;
                self.write_path(&path, value, target)
            }
        }
    }

    pub(crate) fn write_local(
        &mut self,
        b: marlin_front_hir_def::store::BindingId,
        value: Value,
    ) -> R<()> {
        let value = self.vouched(b, value)?;
        match self.local(b)? {
            Local::Var(var) => self.write_var(var, value),
            Local::Ptr(p) => {
                self.emit(Op::Store { aligned: None }, vec![p, value], None)?;
            }
        }
        Ok(())
    }

    /// `value` as written to local `b`: asserted uniform if `b` is `[Uniform]`.
    fn vouched(&mut self, b: marlin_front_hir_def::store::BindingId, value: Value) -> R<Value> {
        if self.uniform_locals.contains(&b) {
            self.op(Op::AssumeUniform, vec![value], None)
        } else {
            Ok(value)
        }
    }

    fn write_path(&mut self, path: &ValuePath, value: Value, e: ExprId) -> R<()> {
        let local = match &path.root {
            Root::Local(b) => (self.local(*b)?, self.binding_ty(*b)),
            Root::This => self
                .this
                .clone()
                .ok_or_else(|| self.error("E1305", "no `this` here", Self::loc(e)))?,
            // A part a pointer cannot reach (a property): the whole variable is rebuilt.
            Root::Item(item) if self.l.shared_global(*item)?.is_some() => {
                let (p, ty) = self.shared_root(*item, e)?;
                (Local::Ptr(p), ty)
            }
            Root::Item(_) => {
                return Err(LowerError::unsupported(
                    "writing a module-level variable",
                    self.owner(),
                    Self::loc(e),
                ));
            }
            Root::ConstParam { .. } => {
                return Err(self.error("E1305", "a constant cannot be assigned", Self::loc(e)));
            }
        };
        let (place, ty) = local;
        let old = if path.steps.is_empty() {
            None
        } else {
            Some(match place {
                Local::Var(var) => self.read_var(var),
                Local::Ptr(p) => self.op(Op::Load { aligned: None }, vec![p], None)?,
            })
        };
        let new = match old {
            Some(old) => self.rebuild(old, &ty, &path.steps, value, e)?,
            None => value,
        };
        let new = match path.root {
            Root::Local(b) => self.vouched(b, new)?,
            _ => new,
        };
        match place {
            Local::Var(var) => self.write_var(var, new),
            Local::Ptr(p) => {
                self.emit(Op::Store { aligned: None }, vec![p, new], None)?;
            }
        }
        Ok(())
    }

    // ---- conversions ----

    /// `v` of type `from` as `to`: numeric conversions per component, nothing between types
    /// that lower to the same IR type (`int` and `uint`, enums and their underlying type).
    pub(crate) fn convert(&mut self, v: Value, from: &Ty, to: &Ty, e: ExprId) -> R<Value> {
        if from == to {
            return Ok(v);
        }
        let target = self.l.ty(to)?;
        let (Some((fnum, fshape)), Some((tnum, tshape))) =
            (self.l.num_shape(from)?, self.l.num_shape(to)?)
        else {
            if self.f.ty(v) == target {
                return Ok(v);
            }
            return Err(self.error(
                "E1305",
                format!(
                    "no conversion from `{}` to `{}`",
                    self.l.show(from),
                    self.l.show(to)
                ),
                Self::loc(e),
            ));
        };
        // A scalar to a vector: broadcast it first.
        let v = if fshape == Shape::Scalar && tshape != Shape::Scalar {
            let lane_ty = self.vector_ty(&Self::vector_element(to), 1);
            let lane = self.convert(v, from, &lane_ty, e)?;
            return self.op(Op::Splat, vec![lane], Some(target));
        } else {
            v
        };
        if fnum.ir() == tnum.ir() && fnum.is_float() == tnum.is_float() {
            return Ok(v);
        }
        let op = match (fnum, tnum) {
            (Num::Bool, _) | (_, Num::Bool) => {
                return Err(self.error(
                    "E1305",
                    "no numeric conversion from or to `bool`",
                    Self::loc(e),
                ));
            }
            (f, t) if f.is_float() && t.is_float() => {
                if t.bits() > f.bits() {
                    ConvertOp::FpExt
                } else {
                    ConvertOp::FpTrunc
                }
            }
            (f, t) if f.is_float() => {
                if t.is_signed() {
                    ConvertOp::FpToSi
                } else {
                    ConvertOp::FpToUi
                }
            }
            (f, t) if t.is_float() => {
                if f.is_signed() {
                    ConvertOp::SiToFp
                } else {
                    ConvertOp::UiToFp
                }
            }
            (f, t) => {
                if t.bits() < f.bits() {
                    ConvertOp::Trunc
                } else if f.is_signed() {
                    ConvertOp::SExt
                } else {
                    ConvertOp::ZExt
                }
            }
        };
        self.op(Op::Convert(op), vec![v], Some(target))
    }

    // ---- operators ----

    /// A value of type `ty` with `like`'s shape: splats a scalar to a vector or matrix.
    fn broadcast(&mut self, v: Value, like: Type) -> R<Value> {
        if self.f.ty(v) == like {
            return Ok(v);
        }
        match self.l.module.data(like).clone() {
            TypeData::Vector(..) => self.op(Op::Splat, vec![v], Some(like)),
            TypeData::Matrix {
                element,
                rows,
                cols,
            } => {
                let col = self.l.module.vector(element, rows);
                let c = self.op(Op::Splat, vec![v], Some(col))?;
                self.op(Op::Construct, vec![c; cols as usize], Some(like))
            }
            _ => Ok(v),
        }
    }

    pub(crate) fn builtin_binary(
        &mut self,
        op: BinaryOp,
        (l, lt): (Value, &Ty),
        (r, rt): (Value, &Ty),
        e: ExprId,
    ) -> R<Value> {
        let (Some((ln, ls)), Some((_, rs))) = (self.l.num_shape(lt)?, self.l.num_shape(rt)?) else {
            return Err(self.error("E1305", "an operator on non-numeric values", Self::loc(e)));
        };
        if op == BinaryOp::Mul
            && matches!(ls, Shape::Matrix(..))
            && matches!(rs, Shape::Matrix(..) | Shape::Vector(_))
        {
            return self.op(Op::MatMul, vec![l, r], None);
        }
        let shifts = matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::UShr);
        let (mut l, mut r) = (l, r);
        if shifts {
            // The count takes the shifted value's type.
            let lscalar = self.vector_ty(&Self::vector_element(lt), 1);
            let lscalar = if ls == Shape::Scalar {
                lt.clone()
            } else {
                lscalar
            };
            let rscalar = if rs == Shape::Scalar {
                rt.clone()
            } else {
                Self::vector_element(rt)
            };
            if rs == Shape::Scalar {
                r = self.convert(r, &rscalar, &lscalar, e)?;
            }
        }
        // Broadcast a scalar against a vector or matrix.
        let (lt_ir, rt_ir) = (self.f.ty(l), self.f.ty(r));
        if ls == Shape::Scalar && rs != Shape::Scalar {
            l = self.broadcast(l, rt_ir)?;
        } else if rs == Shape::Scalar && ls != Shape::Scalar {
            r = self.broadcast(r, lt_ir)?;
        }
        let f = ln.is_float();
        let s = ln.is_signed();
        let cmp = |icmp: IntCmp, fcmp: FloatCmp| if f { Op::Fcmp(fcmp) } else { Op::Icmp(icmp) };
        let bin = |int: IrBin, float: IrBin| Op::Binary(if f { float } else { int });
        let ir = match op {
            BinaryOp::Add => bin(IrBin::Add, IrBin::FAdd),
            BinaryOp::Sub => bin(IrBin::Sub, IrBin::FSub),
            BinaryOp::Mul => bin(IrBin::Mul, IrBin::FMul),
            BinaryOp::Div => bin(if s { IrBin::SDiv } else { IrBin::UDiv }, IrBin::FDiv),
            BinaryOp::Rem => bin(if s { IrBin::SRem } else { IrBin::URem }, IrBin::FRem),
            BinaryOp::BitAnd => Op::Binary(IrBin::And),
            BinaryOp::BitOr => Op::Binary(IrBin::Or),
            BinaryOp::BitXor => Op::Binary(IrBin::Xor),
            BinaryOp::Shl => Op::Binary(IrBin::Shl),
            BinaryOp::Shr => Op::Binary(if s { IrBin::AShr } else { IrBin::LShr }),
            BinaryOp::UShr => Op::Binary(IrBin::LShr),
            BinaryOp::Eq => cmp(IntCmp::Eq, FloatCmp::Oeq),
            BinaryOp::Ne => cmp(IntCmp::Ne, FloatCmp::Une),
            BinaryOp::Lt => cmp(if s { IntCmp::Slt } else { IntCmp::Ult }, FloatCmp::Olt),
            BinaryOp::Le => cmp(if s { IntCmp::Sle } else { IntCmp::Ule }, FloatCmp::Ole),
            BinaryOp::Gt => cmp(if s { IntCmp::Sgt } else { IntCmp::Ugt }, FloatCmp::Ogt),
            BinaryOp::Ge => cmp(if s { IntCmp::Sge } else { IntCmp::Uge }, FloatCmp::Oge),
            BinaryOp::And | BinaryOp::Or => {
                unreachable!("short-circuit operators are lowered apart")
            }
        };
        self.op(ir, vec![l, r], None)
    }

    /// The declared operator `e` calls, if it calls one: resolved on the type of its first
    /// operand, so an interface operator on a scalar or vector is the built-in one.
    fn user_operator(
        &mut self,
        e: ExprId,
        first: ExprId,
    ) -> R<Option<(marlin_front_hir_def::ItemId, Subst)>> {
        let Some(Callee::Item { item, subst }) = self.infer.callees.get(e).cloned() else {
            return Ok(None);
        };
        let subst = self.concrete_subst(&subst);
        let on = self.final_ty(first);
        Ok(
            match self.resolve_callee(item, &subst, Some(&on), Self::loc(e))? {
                Resolved::Item(item, subst) => Some((item, subst)),
                Resolved::Builtin => None,
            },
        )
    }

    fn binary(&mut self, e: ExprId, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> R<Value> {
        let user = self.user_operator(e, lhs)?;
        let (l, r) = (self.value(lhs)?, self.value(rhs)?);
        match user {
            Some((item, subst)) => self
                .call_function(
                    e,
                    item,
                    &subst,
                    None,
                    vec![CallArg::Value(l), CallArg::Value(r)],
                )?
                .ok_or_else(|| self.error("E1305", "an operator without a result", Self::loc(e))),
            None => {
                let (lt, rt) = (self.final_ty(lhs), self.final_ty(rhs));
                self.builtin_binary(op, (l, &lt), (r, &rt), e)
            }
        }
    }

    fn short_circuit(&mut self, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> R<Value> {
        let a = self.value(lhs)?;
        let bool_ty = self.f.ty(a);
        let var = self.ssa.new_var(bool_ty, None);
        self.write_var(var, a);
        let (rest, merge) = (self.new_block(), self.new_block());
        let (then_block, else_block) = match op {
            BinaryOp::And => (rest, merge),
            _ => (merge, rest),
        };
        self.terminate(Term::CondBr {
            cond: a,
            then_block,
            else_block,
            merge,
        });
        self.seal(rest);
        self.enter(rest);
        let b = self.value(rhs)?;
        self.write_var(var, b);
        self.terminate(Term::Br(merge));
        self.seal(merge);
        self.enter(merge);
        Ok(self.read_var(var))
    }

    fn conditional(
        &mut self,
        e: ExprId,
        cond: ExprId,
        then: ExprId,
        otherwise: ExprId,
    ) -> R<Value> {
        let c = self.value(cond)?;
        let t = self.l.ty(&self.expr_ty(e))?;
        let var = self.ssa.new_var(t, None);
        let (tb, eb, merge) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(Term::CondBr {
            cond: c,
            then_block: tb,
            else_block: eb,
            merge,
        });
        for (block, x) in [(tb, then), (eb, otherwise)] {
            self.seal(block);
            self.enter(block);
            let v = self.value(x)?;
            self.write_var(var, v);
            self.terminate(Term::Br(merge));
        }
        self.seal(merge);
        self.enter(merge);
        Ok(self.read_var(var))
    }

    fn unary(&mut self, e: ExprId, op: UnaryOp, operand: ExprId) -> R<Value> {
        if let Some((item, subst)) = self.user_operator(e, operand)? {
            let a = self.value(operand)?;
            return self
                .call_function(e, item, &subst, None, vec![CallArg::Value(a)])?
                .ok_or_else(|| self.error("E1305", "an operator without a result", Self::loc(e)));
        }
        let result_ty = self.expr_ty(e);
        match op {
            UnaryOp::PreInc | UnaryOp::PreDec | UnaryOp::PostInc | UnaryOp::PostDec => {
                let old = self.value(operand)?;
                let ty = self.final_ty(operand);
                let one = self.one_of(&ty)?;
                let bop = if matches!(op, UnaryOp::PreInc | UnaryOp::PostInc) {
                    BinaryOp::Add
                } else {
                    BinaryOp::Sub
                };
                let new = self.builtin_binary(bop, (old, &ty), (one, &ty), e)?;
                self.assign_place(operand, new)?;
                Ok(if matches!(op, UnaryOp::PreInc | UnaryOp::PreDec) {
                    new
                } else {
                    old
                })
            }
            _ => {
                let v = self.value(operand)?;
                let from = self.final_ty(operand);
                let v = self.convert(v, &from, &result_ty, e)?;
                let num = self.l.num(&result_ty)?.unwrap_or(Num::Signed(32));
                match op {
                    UnaryOp::Plus => Ok(v),
                    UnaryOp::Neg if num.is_float() => self.op(Op::Unary(IrUn::FNeg), vec![v], None),
                    UnaryOp::Neg => self.op(Op::Unary(IrUn::Neg), vec![v], None),
                    _ => self.op(Op::Unary(IrUn::Not), vec![v], None),
                }
            }
        }
    }

    fn assign(
        &mut self,
        e: ExprId,
        op: Option<BinaryOp>,
        target: ExprId,
        value: ExprId,
    ) -> R<Value> {
        // A place in memory: its address once, before the value (C#'s order).
        if self.is_mem_place(target)? {
            let (p, _) = self.mem_place(target)?;
            let new = match op {
                None => self.value(value)?,
                Some(op) => {
                    let old = self.op(Op::Load { aligned: None }, vec![p], None)?;
                    self.compound(e, op, target, old, value)?
                }
            };
            self.emit(Op::Store { aligned: None }, vec![p, new], None)?;
            return Ok(new);
        }
        let new = match op {
            None => {
                if let Expr::DeclTuple(_) | Expr::Tuple(_) = self.body.store.exprs[target] {
                    let v = self.value(value)?;
                    self.assign_place(target, v)?;
                    return Ok(v);
                }
                self.value(value)?
            }
            Some(op) => {
                let old = self.value(target)?;
                self.compound(e, op, target, old, value)?
            }
        };
        self.assign_place(target, new)?;
        Ok(new)
    }

    /// `target op= value` with the target's current value `old`: the new value.
    fn compound(
        &mut self,
        e: ExprId,
        op: BinaryOp,
        target: ExprId,
        old: Value,
        value: ExprId,
    ) -> R<Value> {
        Ok(match self.user_operator(e, target)? {
            Some((item, subst)) => {
                let (a, b) = (old, self.value(value)?);
                self.call_function(
                    e,
                    item,
                    &subst,
                    None,
                    vec![CallArg::Value(a), CallArg::Value(b)],
                )?
                .ok_or_else(|| self.error("E1305", "an operator without a result", Self::loc(e)))?
            }
            _ => {
                let (l, r) = (old, self.value(value)?);
                let (lt, rt) = (self.final_ty(target), self.final_ty(value));
                let v = self.builtin_binary(op, (l, &lt), (r, &rt), e)?;
                // The result has the left operand's type; it converts to the target's.
                let tt = self.expr_ty(target);
                self.convert(v, &lt, &tt, e)?
            }
        })
    }

    // ---- aggregates ----

    fn field_inits(&mut self, mut v: Value, ty: &Ty, inits: &[FieldInit], e: ExprId) -> R<Value> {
        let (_, fields) = self.l.struct_of(ty)?;
        let tree_files: Vec<_> = fields.clone();
        for init in inits {
            let mut found = None;
            for (i, f) in tree_files.iter().enumerate() {
                let tree = self.l.cx.get::<ItemTreeQuery>(&f.file)?;
                if tree.items[f.local].name.as_ref() == Some(&init.name) {
                    found = Some(i as u32);
                }
            }
            let i = found.ok_or_else(|| {
                LowerError::unsupported(
                    format!("initializing `{}`", init.name),
                    self.owner(),
                    Self::loc(e),
                )
            })?;
            let x = self.value(init.value)?;
            v = self.op(Op::InsertField(i), vec![v, x], None)?;
        }
        Ok(v)
    }

    /// A struct with its field defaults (zero where a field has none); zero for other types.
    pub(crate) fn default_value(&mut self, ty: &Ty, loc: InferLoc) -> R<Value> {
        let t = self
            .l
            .ty(ty)
            .map_err(|err| err.located(self.owner(), loc.clone()))?;
        let c = self
            .l
            .default_const(ty, t)
            .map_err(|err| err.located(self.owner(), loc))?;
        Ok(self.f.const_value(c, t))
    }

    // ---- indexing ----

    /// `base[args]` through a temporary variable: stored, accessed, loaded.
    fn through_memory(&mut self, base: Value, indices: Vec<Value>) -> R<(Value, Value)> {
        let t = self.f.ty(base);
        let var = self.temp_var(t)?;
        self.emit(Op::Store { aligned: None }, vec![var, base], None)?;
        let steps = vec![AccessStep::Index; indices.len()];
        let mut operands = vec![var];
        operands.extend(indices);
        let p = self.op(Op::Access(steps), operands, None)?;
        Ok((var, p))
    }

    pub(crate) fn indices(&mut self, base_ty: &Ty, args: &[Arg]) -> R<Vec<Value>> {
        let mut out = Vec::new();
        for a in args {
            out.push(self.value(a.expr)?);
        }
        // `m[r, c]`: the column comes first in memory.
        if self
            .l
            .num_shape(base_ty)?
            .is_some_and(|(_, s)| matches!(s, Shape::Matrix(..)))
        {
            out.reverse();
        }
        Ok(out)
    }

    fn index_read(&mut self, e: ExprId, base: ExprId, args: &[Arg]) -> R<Value> {
        if let Some(Callee::Item { item, subst }) = self.infer.callees.get(e).cloned() {
            let subst = self.concrete_subst(&subst);
            let b = self.value(base)?;
            return self.indexer_get(e, b, args, item, &subst);
        }
        let b = self.value(base)?;
        let bty = self.final_ty(base);
        let idx = self.indices(&bty, args)?;
        match self.l.module.data(self.f.ty(b)).clone() {
            TypeData::Vector(..) => match self.const_bits(idx[0]) {
                Some(i) => self.op(Op::Extract(i as u32), vec![b], None),
                None => self.op(Op::ExtractDyn, vec![b, idx[0]], None),
            },
            TypeData::Array(..) if idx.len() == 1 && self.const_bits(idx[0]).is_some() => {
                let i = self.const_bits(idx[0]).unwrap_or(0);
                self.op(Op::ExtractElem(i as u32), vec![b], None)
            }
            _ => {
                let (_, p) = self.through_memory(b, idx)?;
                self.op(Op::Load { aligned: None }, vec![p], None)
            }
        }
    }

    fn index_write(&mut self, e: ExprId, base: ExprId, args: &[Arg], value: Value) -> R<()> {
        if let Some(Callee::Item { item, subst }) = self.infer.callees.get(e).cloned() {
            let subst = self.concrete_subst(&subst);
            let b = self.value(base)?;
            let new = self.indexer_set(e, b, args, value, item, &subst)?;
            return self.assign_place(base, new);
        }
        let b = self.value(base)?;
        let bty = self.final_ty(base);
        let idx = self.indices(&bty, args)?;
        let new = match self.l.module.data(self.f.ty(b)).clone() {
            TypeData::Vector(..) => match self.const_bits(idx[0]) {
                Some(i) => self.op(Op::Insert(i as u32), vec![b, value], None)?,
                None => self.op(Op::InsertDyn, vec![b, idx[0], value], None)?,
            },
            _ => {
                let (var, p) = self.through_memory(b, idx)?;
                self.emit(Op::Store { aligned: None }, vec![p, value], None)?;
                self.op(Op::Load { aligned: None }, vec![var], None)?
            }
        };
        self.assign_place(base, new)
    }

    // ---- calls ----

    fn call(&mut self, e: ExprId, callee: ExprId, args: &[Arg]) -> R<Option<Value>> {
        match self.infer.callees.get(e).cloned() {
            Some(Callee::Item { item, subst }) => self.call_item(e, callee, args, item, &subst),
            Some(Callee::BuiltinConstruct(ty)) => {
                let ty = self.concrete(&ty);
                let t = self.l.ty(&ty)?;
                let mut parts = Vec::new();
                for a in args {
                    parts.push(self.value(a.expr)?);
                }
                match (self.l.module.data(t).clone(), parts.as_slice()) {
                    (TypeData::Vector(..), [one])
                        if self.l.module.width(self.f.ty(*one)) == Some(1) =>
                    {
                        self.op(Op::Splat, vec![*one], Some(t)).map(Some)
                    }
                    (TypeData::Vector(..), _) => self.op(Op::Construct, parts, Some(t)).map(Some),
                    (_, [one]) => Ok(Some(*one)),
                    _ => Err(self.error("E1305", "a construction that does not fit", Self::loc(e))),
                }
            }
            Some(Callee::DefaultConstruct(ty)) => {
                let ty = self.concrete(&ty);
                self.default_value(&ty, Self::loc(e)).map(Some)
            }
            Some(Callee::Invoke(b)) => self.invoke(e, b, args),
            _ => Err(self.error("E1305", "a call to nothing", Self::loc(e))),
        }
    }

    /// The callee's substitution with this instance's arguments put in.
    pub(crate) fn concrete_subst(&self, subst: &Subst) -> Subst {
        let mut s = subst.clone();
        for (_, args) in &mut s.entries {
            for a in args.iter_mut().flatten() {
                *a = self.subst().arg(a);
            }
        }
        s
    }

    fn call_item(
        &mut self,
        e: ExprId,
        callee: ExprId,
        args: &[Arg],
        item: marlin_front_hir_def::ItemId,
        subst: &Subst,
    ) -> R<Option<Value>> {
        let subst = self.concrete_subst(subst);
        // An interface member: the one the concrete receiver type implements.
        let on = self.infer.receivers.get(e).map(|t| self.concrete(t));
        let (item, subst) = match self.resolve_callee(item, &subst, on.as_ref(), Self::loc(e))? {
            Resolved::Item(i, s) => (i, s),
            Resolved::Builtin => {
                return Err(LowerError::unsupported(
                    "this call on a built-in type",
                    self.owner(),
                    Self::loc(e),
                ));
            }
        };
        // Arguments in source order.
        let mut evaluated: Vec<Option<CallArg>> = Vec::new();
        // The receiver first, for instance methods.
        let receiver = self.receiver(callee)?;
        let types = self.l.cx.get::<ItemTypesQuery>(&item)?;
        if let Some(op) = types.intrinsic.clone() {
            return self.intrinsic(e, &op, item, &subst, receiver, args);
        }
        // Compile-time evaluation calls only `const` functions (0004 §13.4).
        if self.l.ctfe {
            let tree = self.l.cx.get::<ItemTreeQuery>(&item.file)?;
            let data = &tree.items[item.local];
            if data.kind == ItemKind::Function && !data.is_const {
                let path = marlin_front_hir_def::item_path(self.l.cx, item)?;
                return Err(self.error(
                    "E1306",
                    format!(
                        "compile-time evaluation calls only `const` functions; `{path}` is not one"
                    ),
                    Self::loc(e),
                ));
            }
        }
        // Arguments that go to `Func`/`Action` parameters pass a closure, not a value.
        let func_args: Vec<bool> = {
            let slots = self
                .infer
                .call_args
                .get(e)
                .cloned()
                .unwrap_or_else(|| (0..args.len() as u32).map(Some).collect());
            let mut out = vec![false; args.len()];
            for (p, slot) in slots.iter().enumerate() {
                if let (Some(a), Some(param)) = (slot, types.params.get(p)) {
                    let ty = self.l.resolve_assoc(&subst.ty(&param.ty));
                    if let Some(x) = out.get_mut(*a as usize) {
                        *x = crate::closures::is_func(self.l, &ty);
                    }
                }
            }
            out
        };
        for (i, a) in args.iter().enumerate() {
            if func_args[i] {
                let (closure, env) = self.closure_arg(a.expr)?;
                evaluated.push(Some(CallArg::Closure(closure, env)));
                continue;
            }
            evaluated.push(Some(match a.modifier {
                Some(RefKind::Out) => CallArg::Out(a.expr),
                Some(RefKind::Ref) => CallArg::Ref(a.expr, self.value(a.expr)?),
                _ => CallArg::Value(self.value(a.expr)?),
            }));
        }
        let slots = self
            .infer
            .call_args
            .get(e)
            .cloned()
            .unwrap_or_else(|| (0..args.len() as u32).map(Some).collect());
        let mut ordered = Vec::new();
        for (p, slot) in slots.iter().enumerate() {
            ordered.push(match slot {
                Some(a) => evaluated[*a as usize]
                    .take()
                    .ok_or_else(|| self.error("E1305", "an argument used twice", Self::loc(e)))?,
                None => CallArg::Value(self.default_arg(item, p, &subst, e)?),
            });
        }
        self.call_function(e, item, &subst, receiver, ordered)
    }

    /// The receiver of an instance-method call: a place (a path) or a value.
    fn receiver(&mut self, callee: ExprId) -> R<Option<Receiver>> {
        Ok(match self.body.store.exprs[callee].clone() {
            Expr::Path(path) => match self.infer.paths.get(callee).cloned() {
                Some(p) => Some(Receiver::Place(p)),
                None if path.segments.len() == 1 && self.this.is_some() => {
                    Some(Receiver::Place(ValuePath {
                        root: Root::This,
                        steps: Vec::new(),
                    }))
                }
                None => None,
            },
            Expr::Member { receiver, .. } => match self.infer.paths.get(receiver).cloned() {
                Some(p) => Some(Receiver::Place(p)),
                None => match self.body.store.exprs[receiver] {
                    Expr::This => Some(Receiver::Place(ValuePath {
                        root: Root::This,
                        steps: Vec::new(),
                    })),
                    _ => Some(Receiver::Value(self.value(receiver)?)),
                },
            },
            _ => None,
        })
    }

    /// Calls a declared function: `this` and the arguments in parameter order.
    pub(crate) fn call_function(
        &mut self,
        e: ExprId,
        item: marlin_front_hir_def::ItemId,
        subst: &Subst,
        receiver: Option<Receiver>,
        args: Vec<CallArg>,
    ) -> R<Option<Value>> {
        let owner = BodyOwner {
            item,
            part: BodyPart::Main,
        };
        Ok(self.call_body(e, owner, subst, receiver, args)?.0)
    }

    /// Calls a body (a function or an accessor): its result, and, for a receiver passed by
    /// value to a `this` taken by pointer, the receiver as the call left it.
    pub(crate) fn call_body(
        &mut self,
        e: ExprId,
        owner: BodyOwner,
        subst: &Subst,
        receiver: Option<Receiver>,
        args: Vec<CallArg>,
    ) -> R<(Option<Value>, Option<Value>)> {
        let closures: Vec<(u32, Instance)> = args
            .iter()
            .enumerate()
            .filter_map(|(i, a)| match a {
                CallArg::Closure(c, _) => Some((i as u32, c.clone())),
                _ => None,
            })
            .collect();
        let instance = Instance {
            owner,
            subst: subst.clone(),
            closures,
            inner: None,
        };
        let mut changed_receiver = None;
        let sig =
            signature(self.l, &instance).map_err(|err| err.located(self.owner(), Self::loc(e)))?;
        let id = self
            .l
            .instance(instance)
            .map_err(|err| err.located(self.owner(), Self::loc(e)))?;
        let mut operands = Vec::new();
        let mut write_back: Vec<(WriteBack, Value)> = Vec::new();
        match (&sig.this, receiver) {
            (Some((_, pass)), Some(r)) => {
                let (v, place) = match r {
                    Receiver::Place(p) => (self.read_path(&p, e)?, Some(p)),
                    Receiver::Value(v) => (v, None),
                };
                match pass {
                    Pass::Value => operands.push(v),
                    Pass::Pointer => {
                        let t = self.f.ty(v);
                        let var = self.temp_var(t)?;
                        self.emit(Op::Store { aligned: None }, vec![var, v], None)?;
                        operands.push(var);
                        match place {
                            Some(p) => write_back.push((WriteBack::Path(p), var)),
                            None => changed_receiver = Some(var),
                        }
                    }
                }
            }
            (Some(_), None) => {
                return Err(self.error(
                    "E1305",
                    "an instance method called without a receiver",
                    Self::loc(e),
                ));
            }
            _ => {}
        }
        for (arg, (ty, _)) in args.into_iter().zip(&sig.params) {
            match arg {
                CallArg::Value(v) | CallArg::Closure(_, v) => operands.push(v),
                CallArg::Out(x) | CallArg::Ref(x, _) => {
                    let t = self.l.ty(ty)?;
                    let var = self.temp_var(t)?;
                    if let CallArg::Ref(_, v) = arg {
                        self.emit(Op::Store { aligned: None }, vec![var, v], None)?;
                    }
                    operands.push(var);
                    write_back.push((WriteBack::Expr(x), var));
                }
            }
        }
        let inst = self.emit(Op::Call(id), operands, None)?;
        let result = self.f.insts[inst].results.first().copied();
        for (target, var) in write_back {
            let v = self.op(Op::Load { aligned: None }, vec![var], None)?;
            match target {
                WriteBack::Expr(x) => self.assign_place(x, v)?,
                WriteBack::Path(p) => self.write_path(&p, v, e)?,
            }
        }
        let changed = match changed_receiver {
            Some(var) => Some(self.op(Op::Load { aligned: None }, vec![var], None)?),
            None => None,
        };
        Ok((result, changed))
    }

    /// A parameter's default (literals for now).
    fn default_arg(
        &mut self,
        item: marlin_front_hir_def::ItemId,
        param: usize,
        subst: &Subst,
        e: ExprId,
    ) -> R<Value> {
        let sig = self.l.cx.get::<ItemSignature>(&item)?;
        let types = self.l.cx.get::<ItemTypesQuery>(&item)?;
        let ty = subst.ty(&types.params[param].ty);
        let t = self.l.ty(&ty)?;
        let Some(d) = sig.params.get(param).and_then(|p| p.default) else {
            return Err(self.error(
                "E1305",
                "a missing argument without a default",
                Self::loc(e),
            ));
        };
        let (negate, lit) = match &sig.store.exprs[d] {
            Expr::Unary {
                op: UnaryOp::Neg,
                operand,
            } => (true, &sig.store.exprs[*operand]),
            other => (false, other),
        };
        match lit {
            Expr::Literal(Literal::Bool(b)) => Ok(self.bool_const(*b)),
            Expr::Literal(Literal::Number(text)) => {
                let text = if negate {
                    format!("-{text}")
                } else {
                    text.to_string()
                };
                let num = self.l.num(&ty)?;
                match num {
                    Some(n) if n.is_float() => {
                        let clean = text.trim_end_matches(['f', 'F', 'd', 'D', 'h', 'H']);
                        let v: f64 = clean
                            .parse()
                            .or_else(|_| parse_int(clean).map(|i| i as f64).ok_or(()))
                            .map_err(|_| {
                                self.error("E1305", "a default that does not parse", Self::loc(e))
                            })?;
                        let c = self.l.module.float(t, v);
                        Ok(self.f.const_value(c, t))
                    }
                    _ => {
                        let v = parse_int(&text).ok_or_else(|| {
                            self.error("E1305", "a default that does not parse", Self::loc(e))
                        })?;
                        Ok(self.int_const(t, v as i64))
                    }
                }
            }
            _ => Err(LowerError::unsupported(
                "a default argument that is not a literal",
                self.owner(),
                Self::loc(e),
            )),
        }
    }

    // ---- intrinsics ----

    pub(crate) fn intrinsic(
        &mut self,
        e: ExprId,
        name: &str,
        item: marlin_front_hir_def::ItemId,
        subst: &Subst,
        receiver: Option<Receiver>,
        args: &[Arg],
    ) -> R<Option<Value>> {
        let _ = subst;
        // `Debug.Print($"...")`: the format goes to the module's table.
        if name == "debug.print"
            && let [arg] = args
            && let Expr::Interpolated(parts) = self.body.store.exprs[arg.expr].clone()
        {
            let mut format = String::new();
            let mut values = Vec::new();
            for part in parts {
                match part {
                    InterpolationPart::Text(t) => format.push_str(&t),
                    InterpolationPart::Hole { expr, .. } => {
                        format.push_str("{}");
                        values.push(self.value(expr)?);
                    }
                }
            }
            let index = self.l.module.formats.len() as u32;
            self.l.module.formats.push(format.into());
            self.emit(Op::DebugPrint(index), values, None)?;
            return Ok(None);
        }
        if let Some(op) = name.strip_prefix("atomic.") {
            let mut operands = Vec::new();
            for a in args {
                operands.push(self.value(a.expr)?);
            }
            return self.atomic(e, op, receiver, operands);
        }
        if name == "buffer.length" {
            let b = match receiver {
                Some(Receiver::Place(p)) => self.read_path(&p, e)?,
                Some(Receiver::Value(v)) => v,
                None => {
                    return Err(self.error(
                        "E1305",
                        "a buffer length without a buffer",
                        Self::loc(e),
                    ));
                }
            };
            let h = self.op(Op::DescGet { nonuniform: false }, vec![b], None)?;
            return self.op(Op::BufferLength, vec![h], None).map(Some);
        }
        let mut operands = Vec::new();
        if let Some(r) = receiver {
            operands.push(match r {
                Receiver::Place(p) => self.read_path(&p, e)?,
                Receiver::Value(v) => v,
            });
        }
        let slots = self
            .infer
            .call_args
            .get(e)
            .cloned()
            .unwrap_or_else(|| (0..args.len() as u32).map(Some).collect());
        let mut evaluated = Vec::new();
        for a in args {
            evaluated.push(Some(self.value(a.expr)?));
        }
        for (p, slot) in slots.iter().enumerate() {
            operands.push(match slot {
                Some(a) => evaluated[*a as usize]
                    .take()
                    .ok_or_else(|| self.error("E1305", "an argument used twice", Self::loc(e)))?,
                None => self.default_arg(item, p, subst, e)?,
            });
        }
        let Some(op) = crate::intrinsic::op(name) else {
            return Err(LowerError::unsupported(
                format!("intrinsic `{name}`"),
                self.owner(),
                Self::loc(e),
            ));
        };
        let inst = self
            .emit(op, operands, None)
            .map_err(|err| err.located(self.owner(), Self::loc(e)))?;
        Ok(self.f.insts[inst].results.first().copied())
    }
}

/// An argument ready to pass.
pub(crate) enum CallArg {
    Value(Value),
    /// A lambda for a `Func`/`Action` parameter, and its environment.
    Closure(Instance, Value),
    /// `out x`: the place written after the call.
    Out(ExprId),
    /// `ref x`: the place, and its value before the call.
    Ref(ExprId, Value),
}

pub(crate) enum Receiver {
    Place(ValuePath),
    Value(Value),
}

enum WriteBack {
    Expr(ExprId),
    Path(ValuePath),
}

impl FnCx<'_, '_, '_> {
    /// `b` is a local `const`.
    pub(crate) fn is_const_local(&self, b: BindingId) -> bool {
        matches!(
            self.body.store.bindings[b].kind,
            BindingKind::Local { is_const: true }
        )
    }

    /// The value of the local `const` `b`, as a constant of this function.
    pub(crate) fn const_local(&mut self, b: BindingId) -> R<Value> {
        let owner = self.owner();
        let subst = self.subst().clone();
        let v = self.l.local_const(owner, &subst, b)?;
        let t = self.l.ty(&v.ty)?;
        let c = self.l.val_const(&v.val, t)?;
        Ok(self.f.const_value(c, self.l.module.consts[c].ty))
    }
}
