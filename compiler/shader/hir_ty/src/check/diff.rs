//! What a `[Differentiable]` function calls (0004 §15.1; decided 2026-10-18). A derivative
//! flows into a call when the call's result has a derivative and so does one of its arguments
//! that is not wrapped in `Detach`. Then the callee must be `[Differentiable]` too (E1138).
//! Operators, constructors and accessors are not checked.

use marlin_front_hir_def::store::{Expr, ExprId};
use marlin_front_hir_def::{BodyPart, ItemKind, ItemTreeQuery};
use marlin_lang_query::Cancelled;

use super::Checker;
use crate::attrs::{ItemAttributesQuery, KnownAttr};
use crate::infer::{Callee, InferLoc};
use crate::item::ItemTypesQuery;

pub(super) fn run(checker: &mut Checker<'_, '_>) -> Result<(), Cancelled> {
    if checker.owner.part != BodyPart::Main {
        return Ok(());
    }
    let cx = checker.cx;
    if !cx
        .get::<ItemAttributesQuery>(&checker.owner.item)?
        .has(KnownAttr::Differentiable)
    {
        return Ok(());
    }
    let body = checker.body.clone();
    for (e, expr) in body.store.exprs.iter() {
        let Expr::Call { args, .. } = expr else {
            continue;
        };
        let Some(Callee::Item { item, .. }) = checker.infer.callees.get(e) else {
            continue;
        };
        let item = *item;
        let tree = cx.get::<ItemTreeQuery>(&item.file)?;
        if tree.items[item.local].kind != ItemKind::Function
            || cx
                .get::<ItemAttributesQuery>(&item)?
                .has(KnownAttr::Differentiable)
        {
            continue;
        }
        let builtins = checker.builtins.clone();
        if !builtins.differentiable(&checker.infer.type_of(e)) {
            continue;
        }
        let mut flows = false;
        for a in args {
            flows |= builtins.differentiable(&checker.infer.type_of(a.expr))
                && !is_detach(checker, a.expr)?;
        }
        if flows {
            let callee = marlin_front_hir_def::item_path(cx, item)?;
            let me = marlin_front_hir_def::item_path(cx, checker.owner.item)?;
            checker.report(
                InferLoc::Expr(e),
                "E1138",
                format!(
                    "`{me}` is `[Differentiable]`, but `{callee}` is not: mark it \
                     `[Differentiable]`, or pass `Detach(x)` to stop the derivative"
                ),
            );
        }
    }
    Ok(())
}

/// Whether `e` is a call of `Detach`.
fn is_detach(checker: &Checker<'_, '_>, e: ExprId) -> Result<bool, Cancelled> {
    if let Some(Callee::Item { item, .. }) = checker.infer.callees.get(e)
        && matches!(checker.body.store.exprs[e], Expr::Call { .. })
    {
        let types = checker.cx.get::<ItemTypesQuery>(item)?;
        return Ok(types.intrinsic.as_deref() == Some("diff.detach"));
    }
    Ok(false)
}
