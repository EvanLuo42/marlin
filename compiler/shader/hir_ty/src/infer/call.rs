//! Calls: overload resolution with C#'s core rules (0004 §9.8), generic inference, argument
//! checking.

use marlin_front_hir_def::store::{Arg, Expr, ExprId, RefKind};
use marlin_front_hir_def::{ItemId, ItemKind, ItemTreeQuery, Name};
use marlin_lang_query::Cancelled;

use super::{Callee, InferCtx, InferLoc, Lit};
use crate::builtins::BuiltinKind;
use crate::item::ItemTypesQuery;
use crate::subst::Subst;
use crate::ty::{ConstTy, GenericArgTy, Ty};

/// One overload as the selection sees it: the type each argument goes to.
#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    pub(crate) params: Vec<Ty>,
    pub(crate) generic: bool,
    pub(crate) defaults_used: usize,
}

pub(crate) enum Pick {
    One(usize),
    NoneApplicable,
    Ambiguous(Vec<usize>),
}

/// An argument as overload resolution sees it before checking.
#[derive(Clone, Debug)]
pub(crate) struct ArgInfo {
    pub(crate) expr: ExprId,
    pub(crate) name: Option<Name>,
    pub(crate) modifier: Option<RefKind>,
    /// `None` for arguments typed only by their target (lambdas, `{ ... }`, `default`, `[...]`).
    pub(crate) ty: Option<Ty>,
    pub(crate) lit: Lit,
}

/// A function overload with arguments mapped to its parameters.
#[derive(Clone, Debug)]
pub(crate) struct Mapped {
    pub(crate) item: ItemId,
    pub(crate) subst: Subst,
    /// For each argument, the (substituted) parameter type it goes to.
    pub(crate) arg_params: Vec<Ty>,
    pub(crate) generic: bool,
    pub(crate) defaults_used: usize,
    /// For each parameter, the argument that goes to it (`None`: its default).
    pub(crate) slots: Vec<Option<usize>>,
}

impl InferCtx<'_, '_> {
    /// The best of `candidates` for arguments of the given types.
    pub(crate) fn pick(
        &self,
        candidates: &[Candidate],
        args: &[(Ty, Lit)],
    ) -> Result<Pick, Cancelled> {
        let mut applicable = Vec::new();
        'candidates: for (i, c) in candidates.iter().enumerate() {
            if c.params.len() != args.len() {
                continue;
            }
            for ((ty, lit), param) in args.iter().zip(&c.params) {
                if !self.implicit(ty, param, *lit)? {
                    continue 'candidates;
                }
            }
            applicable.push(i);
        }
        if applicable.is_empty() {
            return Ok(Pick::NoneApplicable);
        }
        let mut best = Vec::new();
        for &i in &applicable {
            let mut beats_all = true;
            for &j in &applicable {
                if i != j && !self.better(&candidates[i], &candidates[j], args)? {
                    beats_all = false;
                    break;
                }
            }
            if beats_all {
                best.push(i);
            }
        }
        Ok(match best.as_slice() {
            [one] => Pick::One(*one),
            _ => Pick::Ambiguous(applicable),
        })
    }

    /// Whether `a` is a better function member than `b` (C# §12.6.4.3, core rules).
    fn better(&self, a: &Candidate, b: &Candidate, args: &[(Ty, Lit)]) -> Result<bool, Cancelled> {
        let mut a_wins = false;
        let mut b_wins = false;
        for ((ty, lit), (pa, pb)) in args.iter().zip(a.params.iter().zip(&b.params)) {
            match self.better_target(ty, *lit, pa, pb)? {
                Some(true) => a_wins = true,
                Some(false) => b_wins = true,
                None => {}
            }
        }
        if a_wins || b_wins {
            return Ok(a_wins && !b_wins);
        }
        if a.generic != b.generic {
            return Ok(!a.generic);
        }
        Ok(a.defaults_used < b.defaults_used)
    }

    /// Maps arguments to `item`'s parameters (positional, then named, then defaults) and infers
    /// its generic parameters. `None` when the call cannot fit this overload.
    pub(crate) fn map_call(
        &mut self,
        item: ItemId,
        outer: &Subst,
        explicit_args: Option<&[GenericArgTy]>,
        args: &[ArgInfo],
    ) -> Result<Option<Mapped>, Cancelled> {
        let types = self.cx.get::<ItemTypesQuery>(&item)?;
        let params = &types.params;
        let mut slots: Vec<Option<usize>> = vec![None; params.len()];
        for (i, arg) in args.iter().enumerate() {
            let index = match &arg.name {
                Some(name) => match params.iter().position(|p| p.name.as_ref() == Some(name)) {
                    Some(p) => p,
                    None => return Ok(None),
                },
                None => {
                    if args[..i].iter().any(|a| a.name.is_some()) {
                        return Ok(None);
                    }
                    i
                }
            };
            if index >= params.len() || slots[index].is_some() {
                return Ok(None);
            }
            slots[index] = Some(i);
        }
        let mut defaults_used = 0;
        for (slot, param) in slots.iter().zip(params.iter()) {
            if slot.is_none() {
                if !param.has_default {
                    return Ok(None);
                }
                defaults_used += 1;
            }
        }

        let mut subst = outer.clone();
        let generics = types.generics.len();
        if generics > 0 {
            match explicit_args {
                Some(explicit) if explicit.len() == generics => {
                    subst = subst.with_args(item, explicit);
                }
                Some(_) => return Ok(None),
                None => subst = subst.with_unknowns(item, generics),
            }
        }
        // Infer from the arguments whose types are known. A parameter whose type is known
        // already (`T` of `Buffer<uint>`, or fixed by an earlier argument) takes its argument
        // by conversion, checked when choosing: `b.AtomicAdd(0, 1)` passes the `int` literal
        // as `uint`.
        for (p, slot) in slots.iter().enumerate() {
            let Some(a) = slot else { continue };
            if !mentions_params_of(&subst.ty(&params[p].ty), item) {
                continue;
            }
            if let Some(ty) = &args[*a].ty {
                let natural = self.natural_for_inference(ty, args[*a].lit);
                if !subst.unify(&params[p].ty, &natural) {
                    return Ok(None);
                }
            }
        }
        if generics > 0 && !subst.complete(item) {
            // Lambdas may still pin down a result type parameter: try them with what is known.
            for (p, slot) in slots.iter().enumerate() {
                let Some(a) = slot else { continue };
                if args[*a].ty.is_none()
                    && let Some(ty) =
                        self.lambda_result_for_inference(args[*a].expr, &subst.ty(&params[p].ty))?
                {
                    subst.unify(&params[p].ty, &ty);
                }
            }
            if !subst.complete(item) {
                return Ok(None);
            }
        }

        let mut arg_params = vec![Ty::Error; args.len()];
        let mut arg_refs = vec![None; args.len()];
        for (p, slot) in slots.iter().enumerate() {
            if let Some(a) = slot {
                arg_params[*a] = crate::item::normalize_assoc(self.cx, &subst.ty(&params[p].ty))?;
                arg_refs[*a] = params[p].ref_kind;
            }
        }
        // `out` and `ref` must be written at the call; `in` may be left out.
        for (a, arg) in args.iter().enumerate() {
            let ok = match (arg.modifier, arg_refs[a]) {
                (None, None) | (None, Some(RefKind::In)) => true,
                (Some(x), Some(y)) => x == y,
                _ => false,
            };
            if !ok {
                return Ok(None);
            }
        }
        Ok(Some(Mapped {
            item,
            subst,
            arg_params,
            generic: generics > 0,
            defaults_used,
            slots,
        }))
    }

    /// The type a literal argument contributes to generic inference: its natural type.
    fn natural_for_inference(&self, ty: &Ty, lit: Lit) -> Ty {
        match lit {
            Lit::None => ty.clone(),
            _ => ty.clone(),
        }
    }

    /// The type a lambda argument produces given its target `Func` type (with parameter types
    /// already known), for inferring the `Func`'s result type parameter.
    fn lambda_result_for_inference(
        &mut self,
        expr: ExprId,
        target: &Ty,
    ) -> Result<Option<Ty>, Cancelled> {
        let Expr::Lambda { .. } = &self.body.store.exprs[expr] else {
            return self.method_group_result_for_inference(expr, target);
        };
        let Some((params, _)) = self.func_signature(target) else {
            return Ok(None);
        };
        if params.iter().any(|p| matches!(p, Ty::Param { .. })) {
            return Ok(None);
        }
        // Type the lambda body speculatively on a copy, then discard the copy's records.
        let saved = self.result.clone();
        let result = self.infer_lambda(expr, target, true)?;
        self.result = saved;
        Ok(result.map(|(_, ret)| {
            let mut t = target.clone();
            if let Ty::Adt { args, .. } = &mut t
                && let Some(last) = args.last()
                && let GenericArgTy::Type(_) = last
            {
                let mut all: Vec<GenericArgTy> = args.to_vec();
                *all.last_mut().unwrap() = GenericArgTy::Type(ret);
                *args = all.into();
            }
            t
        }))
    }

    /// `(parameter types, result type)` of a `Func<..., R>` or `Action<...>`.
    pub(crate) fn func_signature(&self, ty: &Ty) -> Option<(Vec<Ty>, Ty)> {
        let Ty::Adt { item, args } = ty else {
            return None;
        };
        let types: Vec<Ty> = args
            .iter()
            .filter_map(|a| match a {
                GenericArgTy::Type(t) => Some(t.clone()),
                GenericArgTy::Const(_) => None,
            })
            .collect();
        match self.builtins.kind(*item)? {
            BuiltinKind::Func => {
                let (ret, params) = types.split_last()?;
                Some((params.to_vec(), ret.clone()))
            }
            BuiltinKind::Action => Some((types, Ty::Void)),
            _ => None,
        }
    }

    /// Collects argument information, inferring arguments that have a type of their own.
    pub(crate) fn arg_infos(&mut self, args: &[Arg]) -> Result<Vec<ArgInfo>, Cancelled> {
        let mut infos = Vec::new();
        for arg in args {
            let target_typed = matches!(
                &self.body.store.exprs[arg.expr],
                Expr::Lambda { .. }
                    | Expr::ObjectInit { ty: None, .. }
                    | Expr::Default(None)
                    | Expr::Collection(_)
                    | Expr::DeclVar { ty: None, .. }
                    | Expr::DeclTuple(_)
            );
            let target_typed = target_typed || self.method_group_items(arg.expr)?.is_some();
            let ty = if target_typed {
                None
            } else {
                Some(self.infer_expr(arg.expr, None)?)
            };
            infos.push(ArgInfo {
                expr: arg.expr,
                name: arg.name.clone(),
                modifier: arg.modifier,
                ty,
                lit: self.literal_of(arg.expr),
            });
        }
        Ok(infos)
    }

    /// Picks among `overloads` (with the substitution their declaring type contributes),
    /// checks the arguments against the winner, and returns its result type.
    pub(crate) fn resolve_call(
        &mut self,
        call: ExprId,
        overloads: &[(ItemId, Subst)],
        explicit_args: Option<&[GenericArgTy]>,
        args: &[ArgInfo],
        what: &str,
    ) -> Result<Ty, Cancelled> {
        let mut mapped = Vec::new();
        for (item, outer) in overloads {
            if let Some(m) = self.map_call(*item, outer, explicit_args, args)? {
                mapped.push(m);
            }
        }
        // Arguments without a type of their own count as convertible for selection.
        let arg_tys: Vec<(Ty, Lit)> = args
            .iter()
            .map(|a| (a.ty.clone().unwrap_or(Ty::Error), a.lit))
            .collect();
        let candidates: Vec<Candidate> = mapped
            .iter()
            .map(|m| Candidate {
                params: m.arg_params.clone(),
                generic: m.generic,
                defaults_used: m.defaults_used,
            })
            .collect();
        let chosen = match self.pick(&candidates, &arg_tys)? {
            Pick::One(i) => mapped.swap_remove(i),
            Pick::NoneApplicable => {
                let shown: Vec<String> = arg_tys
                    .iter()
                    .zip(args)
                    .map(|((t, _), a)| match &a.ty {
                        Some(_) => self.show(t),
                        None => "_".into(),
                    })
                    .collect();
                let message = if overloads.is_empty() {
                    format!("{what} has no overload")
                } else {
                    format!("no overload of {what} takes ({})", shown.join(", "))
                };
                self.report(InferLoc::Expr(call), "E1104", message);
                for a in args {
                    if a.ty.is_none() {
                        self.infer_expr(a.expr, None)?;
                    }
                }
                return Ok(Ty::Error);
            }
            Pick::Ambiguous(options) => {
                let mut names = Vec::new();
                for i in options {
                    names.push(marlin_front_hir_def::item_path(self.cx, mapped[i].item)?);
                }
                self.report(
                    InferLoc::Expr(call),
                    "E1105",
                    format!(
                        "the call to {what} is ambiguous between {}",
                        names.join(" and ")
                    ),
                );
                return Ok(Ty::Error);
            }
        };

        for (a, arg) in args.iter().enumerate() {
            let target = chosen.arg_params[a].clone();
            self.check_arg(arg, &target)?;
        }
        let types = self.cx.get::<ItemTypesQuery>(&chosen.item)?;
        let kind = self.item_kind(chosen.item)?;
        let _ = kind;
        // Associated types of the concrete arguments become what they stand for.
        let ret = match types.ty.clone() {
            Some(t) => Some(crate::item::normalize_assoc(self.cx, &chosen.subst.ty(&t))?),
            None => None,
        };
        self.result.call_args.insert(
            call,
            chosen.slots.iter().map(|s| s.map(|a| a as u32)).collect(),
        );
        self.result.callees.insert(
            call,
            Callee::Item {
                item: chosen.item,
                subst: chosen.subst,
            },
        );
        Ok(ret.unwrap_or(Ty::Void))
    }

    /// Checks one argument against the parameter type it goes to.
    pub(crate) fn check_arg(&mut self, arg: &ArgInfo, target: &Ty) -> Result<(), Cancelled> {
        match (&self.body.store.exprs[arg.expr], arg.modifier) {
            // `out var x` takes the parameter's type.
            (Expr::DeclVar { ty: None, binding }, Some(RefKind::Out)) => {
                let binding = *binding;
                self.result.binding_types.insert(binding, target.clone());
                self.record(arg.expr, target.clone());
            }
            (_, Some(RefKind::Out | RefKind::Ref)) => {
                let ty = match &arg.ty {
                    Some(t) => t.clone(),
                    None => self.infer_expr(arg.expr, Some(target))?,
                };
                // By-reference arguments must have exactly the parameter's type.
                if ty != *target && !ty.is_error() && !target.is_error() {
                    let (shown, expected) = (self.show(&ty), self.show(target));
                    self.report(
                        InferLoc::Expr(arg.expr),
                        "E1106",
                        format!("a `ref` or `out` argument must be `{expected}`, not `{shown}`"),
                    );
                }
                self.check_place(arg.expr)?;
            }
            _ => match &arg.ty {
                Some(ty) => {
                    let ty = ty.clone();
                    self.coerce(arg.expr, &ty, target)?;
                }
                None => {
                    self.check_expr(arg.expr, target)?;
                }
            },
        }
        Ok(())
    }

    /// The constructors of struct `item` (with the type's arguments), or none if it declares
    /// none.
    pub(crate) fn constructors(
        &self,
        item: ItemId,
        args: &[GenericArgTy],
    ) -> Result<Vec<(ItemId, Subst)>, Cancelled> {
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        let subst = Subst::new().with_args(item, args);
        Ok(tree.items[item.local]
            .members
            .iter()
            .filter(|&&m| tree.items[m].kind == ItemKind::Constructor)
            .map(|&m| {
                (
                    ItemId {
                        file: item.file,
                        local: m,
                    },
                    subst.clone(),
                )
            })
            .collect())
    }
}

/// Whether `ty` still names a generic parameter of `owner` (one inference has not fixed).
fn mentions_params_of(ty: &Ty, owner: ItemId) -> bool {
    match ty {
        Ty::Param { owner: o, .. } => *o == owner,
        Ty::Adt { args, .. } => args.iter().any(|a| match a {
            GenericArgTy::Type(t) => mentions_params_of(t, owner),
            GenericArgTy::Const(c) => matches!(c, ConstTy::Param { owner: o, .. } if *o == owner),
        }),
        Ty::Array { element, len } => {
            mentions_params_of(element, owner)
                || matches!(len, ConstTy::Param { owner: o, .. } if *o == owner)
        }
        Ty::Tuple(items) => items.iter().any(|t| mentions_params_of(t, owner)),
        Ty::Assoc { base, .. } => mentions_params_of(base, owner),
        Ty::Error | Ty::Void => false,
    }
}
