//! `ForwardDiff(f)(args)` (0004 §15.2): the derivative of a `[Differentiable]` function, called
//! right away. Each parameter that takes part in differentiation (one of a differentiable type,
//! without `[NoDiff]`) takes a `DiffPair` of its type; the others take their own type. The
//! result is a `DiffPair` of the function's result. Overloads and type arguments are chosen as
//! for a call of `f` with the `DiffPair`s' values.

use marlin_front_hir_def::store::{Arg, Expr, ExprId, RefKind};
use marlin_front_hir_def::{ItemKind, ItemTreeQuery, Resolution};
use marlin_lang_query::Cancelled;

use super::call::{ArgInfo, Candidate, Pick};
use super::{Callee, InferCtx, InferLoc};
use crate::attrs::{ItemAttributesQuery, KnownAttr};
use crate::item::ItemTypesQuery;
use crate::subst::Subst;
use crate::ty::Ty;

/// The intrinsic name of `ForwardDiff`.
const FORWARD_DIFF: &str = "diff.forward";

impl InferCtx<'_, '_> {
    /// Whether `callee` (a path) names `ForwardDiff`.
    pub(crate) fn names_forward_diff(&mut self, callee: ExprId) -> Result<bool, Cancelled> {
        let Expr::Path(path) = &self.body.store.exprs[callee] else {
            return Ok(false);
        };
        let def_path = path.to_def_path();
        let Some(scope) = self.scopes.scope_of_expr(callee) else {
            return Ok(false);
        };
        let Resolution::Values(items) =
            self.resolver
                .resolve_in_body(self.cx, &def_path, true, &self.scopes, scope)?
        else {
            return Ok(false);
        };
        for item in items.iter() {
            let types = self.cx.get::<ItemTypesQuery>(item)?;
            if types.intrinsic.as_deref() == Some(FORWARD_DIFF) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `ForwardDiff(f)` not called right away.
    pub(crate) fn forward_diff_alone(&mut self, e: ExprId, args: &[Arg]) -> Result<Ty, Cancelled> {
        for a in args {
            if self.method_group_items(a.expr)?.is_none() {
                self.infer_expr(a.expr, None)?;
            }
        }
        self.report(
            InferLoc::Expr(e),
            "E1137",
            "`ForwardDiff(f)` is called right away: `ForwardDiff(f)(DiffPair(x, dx), ...)`",
        );
        Ok(Ty::Error)
    }

    /// `ForwardDiff(f)(args)`: `inner` is `ForwardDiff(f)`.
    pub(crate) fn forward_diff(
        &mut self,
        e: ExprId,
        inner: ExprId,
        inner_args: &[Arg],
        args: &[Arg],
    ) -> Result<Ty, Cancelled> {
        self.record(inner, Ty::Void);
        let fail = |s: &mut Self, at: ExprId, message: String| -> Result<Ty, Cancelled> {
            s.report(InferLoc::Expr(at), "E1137", message);
            Ok(Ty::Error)
        };
        let [group] = inner_args else {
            self.arg_infos(args)?;
            return fail(self, inner, "`ForwardDiff` takes one function".into());
        };
        let Some(items) = self.method_group_items(group.expr)? else {
            self.infer_expr(group.expr, None)?;
            self.arg_infos(args)?;
            return fail(
                self,
                group.expr,
                "`ForwardDiff` takes a function by its name, without calling it".into(),
            );
        };
        let name = match &self.body.store.exprs[group.expr] {
            Expr::Path(p) => format!("`{}`", p.to_def_path()),
            _ => "the function".into(),
        };
        let infos = self.arg_infos(args)?;

        // The overload that takes the `DiffPair`s' values.
        let values: Vec<ArgInfo> = infos
            .iter()
            .map(|a| {
                let mut a = a.clone();
                if let Some(t) = a.ty.as_ref().and_then(|t| self.builtins.diff_pair_of(t)) {
                    a.ty = Some(t);
                }
                a
            })
            .collect();
        let mut mapped = Vec::new();
        for &item in &items {
            if let Some(m) = self.map_call(item, &Subst::new(), None, &values)? {
                mapped.push(m);
            }
        }
        let arg_tys: Vec<_> = values
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
                let shown: Vec<String> = infos
                    .iter()
                    .map(|a| match &a.ty {
                        Some(t) => self.show(t),
                        None => "_".into(),
                    })
                    .collect();
                return fail(
                    self,
                    e,
                    format!(
                        "no overload of {name} takes ({}) as `ForwardDiff` passes it (a \
                         `DiffPair` for each parameter that takes part)",
                        shown.join(", ")
                    ),
                );
            }
            Pick::Ambiguous(_) => {
                return fail(self, e, format!("`ForwardDiff` of {name} is ambiguous"));
            }
        };

        let item = chosen.item;
        let types = self.cx.get::<ItemTypesQuery>(&item)?;
        let attrs = self.cx.get::<ItemAttributesQuery>(&item)?;
        if !attrs.has(KnownAttr::Differentiable) {
            return fail(
                self,
                group.expr,
                format!(
                    "{name} is not `[Differentiable]`, so `ForwardDiff` cannot differentiate it"
                ),
            );
        }
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        let data = &tree.items[item.local];
        let instance = data
            .parent
            .is_some_and(|p| tree.items[p].kind != ItemKind::Namespace)
            && !data.is_static;
        if instance {
            return fail(
                self,
                group.expr,
                format!("{name} is an instance method; `ForwardDiff` takes static functions"),
            );
        }
        let ret = match types.ty.clone() {
            Some(t) => crate::item::normalize_assoc(self.cx, &chosen.subst.ty(&t))?,
            None => Ty::Void,
        };
        if !self.builtins.differentiable(&ret) {
            let shown = self.show(&ret);
            return fail(
                self,
                group.expr,
                format!(
                    "{name} returns `{shown}`, which has no derivative for `ForwardDiff` to give"
                ),
            );
        }
        for (p, slot) in chosen.slots.iter().enumerate() {
            let param = &types.params[p];
            if matches!(param.ref_kind, Some(RefKind::Ref | RefKind::Out)) {
                return fail(
                    self,
                    group.expr,
                    format!(
                        "{name} has `ref` or `out` parameters, which `ForwardDiff` does not take"
                    ),
                );
            }
            let Some(a) = *slot else { continue };
            let pty = chosen.arg_params[a].clone();
            let takes_part = self.builtins.differentiable(&pty)
                && !attrs
                    .params
                    .get(p)
                    .is_some_and(|ps| ps.iter().any(|x| x.known == Some(KnownAttr::NoDiff)));
            let target = if takes_part {
                self.builtins.diff_pair(pty)
            } else {
                pty
            };
            self.check_arg(&infos[a], &target)?;
        }
        self.result
            .method_groups
            .insert(group.expr, (item, chosen.subst.clone()));
        self.record(group.expr, Ty::Void);
        self.result.call_args.insert(
            e,
            chosen.slots.iter().map(|s| s.map(|a| a as u32)).collect(),
        );
        self.result.callees.insert(
            e,
            Callee::ForwardDiff {
                item,
                subst: chosen.subst,
            },
        );
        Ok(self.builtins.diff_pair(ret))
    }
}
