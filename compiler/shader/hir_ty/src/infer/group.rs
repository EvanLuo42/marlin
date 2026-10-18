//! Method groups: a function named without a call (`Integrate(2, Square)`) where a `Func` or
//! `Action` is expected (0004 §9.2). The overload whose parameters and result are exactly the
//! target's is the one passed.

use marlin_front_hir_def::store::{Expr, ExprId};
use marlin_front_hir_def::{ItemId, ItemKind, ItemTreeQuery, Resolution};
use marlin_lang_query::Cancelled;

use super::{InferCtx, InferLoc};
use crate::item::ItemTypesQuery;
use crate::subst::Subst;
use crate::ty::{GenericArgTy, Ty};

impl InferCtx<'_, '_> {
    /// The functions a path expression names, when it names only functions.
    pub(crate) fn method_group_items(
        &mut self,
        e: ExprId,
    ) -> Result<Option<Vec<ItemId>>, Cancelled> {
        let Expr::Path(path) = &self.body.store.exprs[e] else {
            return Ok(None);
        };
        let def_path = path.to_def_path();
        let Some(scope) = self.scopes.scope_of_expr(e) else {
            return Ok(None);
        };
        let Resolution::Values(items) =
            self.resolver
                .resolve_in_body(self.cx, &def_path, true, &self.scopes, scope)?
        else {
            return Ok(None);
        };
        for item in &items {
            let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
            if tree.items[item.local].kind != ItemKind::Function {
                return Ok(None);
            }
        }
        Ok((!items.is_empty()).then(|| items.to_vec()))
    }

    /// The overloads among `items` that match the `Func`/`Action` type `target`, with their
    /// type arguments. A target result that is still a type parameter matches any result.
    fn method_group_matches(
        &mut self,
        items: &[ItemId],
        target: &Ty,
    ) -> Result<Vec<(ItemId, Subst, Ty)>, Cancelled> {
        let Some((params, ret)) = self.func_signature(target) else {
            return Ok(Vec::new());
        };
        let mut found = Vec::new();
        for &item in items {
            let types = self.cx.get::<ItemTypesQuery>(&item)?;
            if types.params.len() != params.len()
                || types.params.iter().any(|p| p.ref_kind.is_some())
            {
                continue;
            }
            let mut s = Subst::new();
            if !types.generics.is_empty() {
                s = s.with_unknowns(item, types.generics.len());
            }
            for (p, t) in types.params.iter().zip(&params) {
                s.unify(&p.ty, t);
            }
            let own_ret = types.ty.clone().unwrap_or(Ty::Void);
            let ret_known = !matches!(ret, Ty::Param { .. });
            if ret_known {
                s.unify(&own_ret, &ret);
            }
            if !s.complete(item) {
                continue;
            }
            let mut exact = true;
            for (p, t) in types.params.iter().zip(&params) {
                exact &= crate::item::normalize_assoc(self.cx, &s.ty(&p.ty))? == *t;
            }
            let own_ret = crate::item::normalize_assoc(self.cx, &s.ty(&own_ret))?;
            if ret_known {
                exact &= own_ret == ret;
            }
            if exact {
                found.push((item, s, own_ret));
            }
        }
        Ok(found)
    }

    /// Types the method group `e` (naming `items`) against `target`.
    pub(crate) fn method_group(
        &mut self,
        e: ExprId,
        items: &[ItemId],
        target: &Ty,
    ) -> Result<Ty, Cancelled> {
        let name = match &self.body.store.exprs[e] {
            Expr::Path(p) => p.to_def_path().to_string(),
            _ => String::new(),
        };
        let found = self.method_group_matches(items, target)?;
        let [(item, subst, _)] = found.as_slice() else {
            let shown = self.show(target);
            if found.is_empty() {
                self.report(
                    InferLoc::Expr(e),
                    "E1110",
                    format!("no overload of `{name}` has the parameters and result of `{shown}`"),
                );
            } else {
                self.report(
                    InferLoc::Expr(e),
                    "E1113",
                    format!("more than one overload of `{name}` matches `{shown}`"),
                );
            }
            return Ok(Ty::Error);
        };
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        let data = &tree.items[item.local];
        let instance = data
            .parent
            .is_some_and(|p| tree.items[p].kind != ItemKind::Namespace)
            && !data.is_static;
        if instance && self.this_ty.is_none() {
            self.report(
                InferLoc::Expr(e),
                "E1118",
                format!("`{name}` is an instance method; there is no `this` here"),
            );
            return Ok(Ty::Error);
        }
        self.result.method_groups.insert(e, (*item, subst.clone()));
        self.record(e, target.clone());
        Ok(target.clone())
    }

    /// The `Func` type a method group gives `target` (with its parameter types known), for
    /// inferring the `Func`'s result type parameter.
    pub(crate) fn method_group_result_for_inference(
        &mut self,
        e: ExprId,
        target: &Ty,
    ) -> Result<Option<Ty>, Cancelled> {
        let Some(items) = self.method_group_items(e)? else {
            return Ok(None);
        };
        let found = self.method_group_matches(&items, target)?;
        let [(_, _, ret)] = found.as_slice() else {
            return Ok(None);
        };
        let mut t = target.clone();
        if let Ty::Adt { args, .. } = &mut t
            && let Some(GenericArgTy::Type(_)) = args.last()
        {
            let mut all: Vec<GenericArgTy> = args.to_vec();
            *all.last_mut().unwrap() = GenericArgTy::Type(ret.clone());
            *args = all.into();
        }
        Ok(Some(t))
    }
}
