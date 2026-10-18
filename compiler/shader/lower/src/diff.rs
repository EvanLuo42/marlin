//! Automatic differentiation's marks (0004 §15, 0005 §4.4 pass 0.3): lowering says what to
//! differentiate and the autodiff pass, after inlining, does it.
//! - `ForwardDiff(f)(args)` gets a channel of its own (`diff.channel`); each `DiffPair`
//!   argument of a parameter that takes part is `diff.seed`ed with its derivative, `f` is
//!   called as usual, and the result's derivative is its `diff.tangent`.
//! - Arguments of `[NoDiff]` parameters are `diff.detach`ed at every call, like `Detach(x)`.

use marlin_front_hir_def::ItemId;
use marlin_front_hir_def::store::{Arg, ExprId};
use marlin_shaderc_hir_ty::{
    InferLoc, ItemAttributes, ItemAttributesQuery, ItemTypesQuery, KnownAttr, Subst,
};
use marlin_shaderc_ir::{Op, Value};

use crate::expr::CallArg;
use crate::func::FnCx;
use crate::{LowerError, R};

/// Whether parameter `p` is `[NoDiff]`.
pub(crate) fn no_diff(attrs: &ItemAttributes, p: usize) -> bool {
    attrs
        .params
        .get(p)
        .is_some_and(|a| a.iter().any(|x| x.known == Some(KnownAttr::NoDiff)))
}

impl FnCx<'_, '_, '_> {
    /// `ForwardDiff(f)(args)`, `f` being `item` with `subst`.
    pub(crate) fn forward_diff(
        &mut self,
        e: ExprId,
        item: ItemId,
        subst: &Subst,
        args: &[Arg],
    ) -> R<Option<Value>> {
        let loc = InferLoc::Expr(e);
        let subst = self.concrete_subst(subst);
        let types = self.l.cx.get::<ItemTypesQuery>(&item)?;
        let attrs = self.l.cx.get::<ItemAttributesQuery>(&item)?;
        let slots = self
            .infer
            .call_args
            .get(e)
            .cloned()
            .unwrap_or_else(|| (0..args.len() as u32).map(Some).collect());
        let channel = self.op(Op::DiffChannel, Vec::new(), None)?;
        // Arguments in source order, then by parameter.
        let mut evaluated = Vec::new();
        for a in args {
            evaluated.push(Some(self.value(a.expr)?));
        }
        let mut values = Vec::new();
        for (p, slot) in slots.iter().enumerate() {
            let v = match slot {
                Some(a) => evaluated[*a as usize]
                    .take()
                    .ok_or_else(|| self.error("E1305", "an argument used twice", loc.clone()))?,
                None => {
                    values.push(self.default_arg(item, p, &subst, e)?);
                    continue;
                }
            };
            let param = self.l.resolve_assoc(&subst.ty(&types.params[p].ty));
            let v = if self.l.builtins.differentiable(&param) && !no_diff(&attrs, p) {
                let x = self.op(Op::ExtractField(0), vec![v], None)?;
                let dx = self.op(Op::ExtractField(1), vec![v], None)?;
                self.op(Op::DiffSeed, vec![channel, x, dx], None)?
            } else if no_diff(&attrs, p) {
                self.op(Op::DiffDetach, vec![v], None)?
            } else {
                v
            };
            values.push(v);
        }
        let result = match types.intrinsic.as_deref() {
            Some(name) => self.intrinsic_values(e, name, item, &subst, values)?,
            None => {
                let args = values.into_iter().map(CallArg::Value).collect();
                self.call_function(e, item, &subst, None, args)?
            }
        };
        let Some(r) = result else {
            return Err(LowerError::unsupported(
                "`ForwardDiff` of a function without a result",
                self.owner(),
                loc,
            ));
        };
        let dr = self.op(Op::DiffTangent, vec![channel, r], None)?;
        let ty = self.concrete(&self.infer.type_of(e));
        let t = self.l.ty(&ty)?;
        self.op(Op::Construct, vec![r, dr], Some(t)).map(Some)
    }
}
