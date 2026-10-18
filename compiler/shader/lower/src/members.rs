//! Properties and indexers: reading calls the getter, writing calls the setter with `this` by
//! pointer (the place it was read from then takes the changed value). Auto-properties, which
//! have no accessor bodies, are not lowered yet.

use marlin_front_hir_def::store::{Arg, BindingKind, ExprId};
use marlin_front_hir_def::{BodyOwner, BodyQuery, ItemId, ItemKind, ItemTreeQuery, body_owners};
use marlin_shaderc_hir_ty::{InferLoc, ItemTypesQuery, Subst, Ty};
use marlin_shaderc_ir::Value;

use crate::expr::{CallArg, Receiver};
use crate::func::FnCx;
use crate::{LowerError, R};

impl FnCx<'_, '_, '_> {
    /// The getter and setter bodies of a property or indexer.
    pub(crate) fn accessors(&self, member: ItemId) -> R<(Option<BodyOwner>, Option<BodyOwner>)> {
        let (mut get, mut set) = (None, None);
        for owner in body_owners(self.l.cx, member)? {
            let body = self.l.cx.get::<BodyQuery>(&owner)?;
            let is_setter = body
                .params
                .iter()
                .any(|&b| body.store.bindings[b].kind == BindingKind::SetterValue);
            if is_setter {
                set = Some(owner);
            } else if get.is_none() {
                get = Some(owner);
            }
        }
        Ok((get, set))
    }

    /// The substitution for a member of the type (or extension block) around it, used on a
    /// value of type `on`.
    pub(crate) fn member_subst(&self, member: ItemId, on: &Ty) -> R<Subst> {
        let tree = self.l.cx.get::<ItemTreeQuery>(&member.file)?;
        let Some(parent) = tree.items[member.local].parent else {
            return Ok(Subst::new());
        };
        let parent_id = ItemId {
            file: member.file,
            local: parent,
        };
        let pdata = &tree.items[parent];
        if pdata.kind == ItemKind::Extension {
            let ext = self.l.cx.get::<ItemTypesQuery>(&parent_id)?;
            let mut s = Subst::new().with_unknowns(parent_id, ext.generics.len());
            if let Some(r) = &ext.receiver {
                s.unify(r, on);
            }
            return Ok(s);
        }
        Ok(match on {
            Ty::Adt { item, args } if *item == parent_id => Subst::new().with_args(*item, args),
            _ => Subst::new(),
        })
    }

    fn is_static(&self, member: ItemId) -> R<bool> {
        let tree = self.l.cx.get::<ItemTreeQuery>(&member.file)?;
        Ok(tree.items[member.local].is_static)
    }

    fn member_ty(&self, member: ItemId, subst: &Subst) -> R<Ty> {
        let types = self.l.cx.get::<ItemTypesQuery>(&member)?;
        Ok(self
            .l
            .resolve_assoc(&subst.ty(types.ty.as_ref().unwrap_or(&Ty::Error))))
    }

    /// `v.P`: the getter's result, and its type.
    pub(crate) fn property_get(
        &mut self,
        p: ItemId,
        v: Value,
        ty: &Ty,
        e: ExprId,
    ) -> R<(Value, Ty)> {
        let loc = InferLoc::Expr(e);
        let subst = self.member_subst(p, ty)?;
        // `b.Length` and the like: an operation, not a getter body.
        if let Some(op) = self.l.cx.get::<ItemTypesQuery>(&p)?.intrinsic.clone() {
            let receiver = (!self.is_static(p)?).then_some(Receiver::Value(v));
            let result = self
                .intrinsic(e, &op, p, &subst, receiver, &[])?
                .ok_or_else(|| self.error("E1305", "a property without a value", loc))?;
            return Ok((result, self.member_ty(p, &subst)?));
        }
        let (get, _) = self.accessors(p)?;
        let get = get.ok_or_else(|| {
            LowerError::unsupported("an auto-property", self.owner(), loc.clone())
        })?;
        let receiver = (!self.is_static(p)?).then_some(Receiver::Value(v));
        let (result, _) = self.call_body(e, get, &subst, receiver, Vec::new())?;
        let result = result.ok_or_else(|| self.error("E1305", "a getter without a result", loc))?;
        Ok((result, self.member_ty(p, &subst)?))
    }

    /// `old.P = value`: the receiver after the setter ran.
    pub(crate) fn property_set(
        &mut self,
        p: ItemId,
        old: Value,
        ty: &Ty,
        value: Value,
        e: ExprId,
    ) -> R<Value> {
        let loc = InferLoc::Expr(e);
        let (_, set) = self.accessors(p)?;
        let set = set.ok_or_else(|| {
            LowerError::unsupported("an auto-property", self.owner(), loc.clone())
        })?;
        let subst = self.member_subst(p, ty)?;
        let (_, changed) = self.call_body(
            e,
            set,
            &subst,
            Some(Receiver::Value(old)),
            vec![CallArg::Value(value)],
        )?;
        // A `readonly` setter (or one of a static property) leaves the receiver as it was.
        Ok(changed.unwrap_or(old))
    }

    /// The arguments of an indexer call, in its parameters' order.
    fn indexer_args(&mut self, e: ExprId, args: &[Arg]) -> R<Vec<CallArg>> {
        let mut evaluated = Vec::new();
        for a in args {
            evaluated.push(Some(self.value(a.expr)?));
        }
        let slots = self
            .infer
            .call_args
            .get(e)
            .cloned()
            .unwrap_or_else(|| (0..args.len() as u32).map(Some).collect());
        let mut out = Vec::new();
        for slot in slots {
            let v = slot
                .and_then(|a| evaluated.get_mut(a as usize).and_then(Option::take))
                .ok_or_else(|| {
                    LowerError::unsupported(
                        "an indexer default argument",
                        self.owner(),
                        InferLoc::Expr(e),
                    )
                })?;
            out.push(CallArg::Value(v));
        }
        Ok(out)
    }

    /// `base[args]` through a declared indexer.
    pub(crate) fn indexer_get(
        &mut self,
        e: ExprId,
        base: Value,
        args: &[Arg],
        item: ItemId,
        subst: &Subst,
    ) -> R<Value> {
        let loc = InferLoc::Expr(e);
        let (get, _) = self.accessors(item)?;
        let get = get.ok_or_else(|| {
            LowerError::unsupported(
                "an indexer without a getter body",
                self.owner(),
                loc.clone(),
            )
        })?;
        let args = self.indexer_args(e, args)?;
        let (result, _) = self.call_body(e, get, subst, Some(Receiver::Value(base)), args)?;
        result.ok_or_else(|| self.error("E1305", "a getter without a result", loc))
    }

    /// `base[args] = value` through a declared indexer: the base after the setter ran.
    pub(crate) fn indexer_set(
        &mut self,
        e: ExprId,
        base: Value,
        args: &[Arg],
        value: Value,
        item: ItemId,
        subst: &Subst,
    ) -> R<Value> {
        let loc = InferLoc::Expr(e);
        let (_, set) = self.accessors(item)?;
        let set = set.ok_or_else(|| {
            LowerError::unsupported("an indexer without a setter body", self.owner(), loc)
        })?;
        let mut args = self.indexer_args(e, args)?;
        args.push(CallArg::Value(value));
        let (_, changed) = self.call_body(e, set, subst, Some(Receiver::Value(base)), args)?;
        Ok(changed.unwrap_or(base))
    }
}
