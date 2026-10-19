//! Interface members called on concrete types (0004 §8.5: no dynamic dispatch): with the type
//! known in each instance, a call to `IShape.Area` on `S` is a call to the member of `S` that
//! implements it, and an interface operator on a scalar or vector is the built-in operator. A
//! member `S` does not implement is the interface's default, its interface arguments those `S`
//! implements it with (`DefaultMaterial : ISurfaceMaterial<DefaultMaterial>`); so is a constant
//! (`M.Blend`) `S` does not declare.

use marlin_front_hir_def::store::ExprId;
use marlin_front_hir_def::{ItemId, ItemKind, ItemSignature, ItemTreeQuery};
use marlin_shaderc_hir_ty::{InferLoc, ItemTypesQuery, Subst, Ty};
use marlin_shaderc_ir::Value;

use crate::func::FnCx;
use crate::{LowerError, R};

pub(crate) enum Resolved {
    Item(ItemId, Subst),
    /// A built-in operator of a scalar, vector or matrix type.
    Builtin,
}

impl FnCx<'_, '_, '_> {
    /// What a call to `item` (with `subst`) runs when the type the call dispatches on (the
    /// receiver, or an operator's first operand) is `on`.
    pub(crate) fn resolve_callee(
        &mut self,
        item: ItemId,
        subst: &Subst,
        on: Option<&Ty>,
        loc: InferLoc,
    ) -> R<Resolved> {
        let tree = self.l.cx.get::<ItemTreeQuery>(&item.file)?;
        let data = &tree.items[item.local];
        let in_interface = data
            .parent
            .is_some_and(|p| tree.items[p].kind == ItemKind::Interface);
        if !in_interface {
            return Ok(Resolved::Item(item, subst.clone()));
        }
        let name = data
            .name
            .clone()
            .map_or_else(|| "operator".to_string(), |n| n.to_string());
        let Some(Ty::Adt {
            item: ty_item,
            args,
        }) = on
        else {
            return Err(LowerError::unsupported(
                format!("calling interface member `{name}` without a concrete type"),
                self.owner(),
                loc,
            ));
        };
        // Operators of scalars, vectors and matrices are the compiler's; their other members
        // (`Zero`, `One`) are declared on the built-in type like any type's.
        let builtin = self.l.builtins.kind(*ty_item).is_some();
        if builtin && data.kind == ItemKind::Operator {
            return Ok(Resolved::Builtin);
        }
        let symbol = self.l.cx.get::<ItemSignature>(&item)?.operator.clone();
        let wanted = self.l.cx.get::<ItemTypesQuery>(&item)?;
        let wanted_params: Vec<Ty> = wanted.params.iter().map(|p| subst.ty(&p.ty)).collect();
        let impl_subst = Subst::new().with_args(*ty_item, args);
        let ttree = self.l.cx.get::<ItemTreeQuery>(&ty_item.file)?;
        let mut candidates = Vec::new();
        for &m in &ttree.items[ty_item.local].members {
            let d = &ttree.items[m];
            if d.kind != data.kind || d.is_static != data.is_static {
                continue;
            }
            let member = ItemId {
                file: ty_item.file,
                local: m,
            };
            let same_name = match data.kind {
                ItemKind::Operator => self.l.cx.get::<ItemSignature>(&member)?.operator == symbol,
                _ => d.name == data.name,
            };
            if !same_name {
                continue;
            }
            let types = self.l.cx.get::<ItemTypesQuery>(&member)?;
            if types.params.len() != wanted_params.len() {
                continue;
            }
            let exact = types
                .params
                .iter()
                .zip(&wanted_params)
                .all(|(p, w)| impl_subst.ty(&p.ty) == self.concrete(w));
            candidates.push((member, exact));
        }
        let chosen = candidates
            .iter()
            .find(|c| c.1)
            .or(candidates.first())
            .map(|c| c.0);
        // Not implemented: the interface's default body, if it has one.
        if chosen.is_none()
            && !builtin
            && !marlin_front_hir_def::body_owners(self.l.cx, item)?.is_empty()
            && let Some(mut s) =
                self.interface_args(item_parent(&tree, item), on.unwrap_or(&Ty::Error))?
        {
            if let Some((_, own)) = subst.entries.iter().find(|(o, _)| *o == item) {
                let own: Vec<_> = own.iter().flatten().cloned().collect();
                if !own.is_empty() {
                    s = s.with_args(item, &own);
                }
            }
            // Its `this` is the type it was reached through.
            s.assoc
                .push((item_parent(&tree, item), on.cloned().unwrap_or(Ty::Error)));
            return Ok(Resolved::Item(item, s));
        }
        let chosen = chosen.ok_or_else(|| {
            let what = if builtin {
                format!("interface member `{name}` of a built-in type that declares none")
            } else {
                format!("the default implementation of interface member `{name}`")
            };
            LowerError::unsupported(what, self.owner(), loc.clone())
        })?;
        // The member's own type arguments carry over by position.
        let mut s = impl_subst;
        if let Some((_, own)) = subst.entries.iter().find(|(o, _)| *o == item) {
            let own: Vec<_> = own.iter().flatten().cloned().collect();
            if !own.is_empty() {
                s = s.with_args(chosen, &own);
            }
        }
        Ok(Resolved::Item(chosen, s))
    }
}

/// The interface `item` (a member of one) belongs to.
fn item_parent(tree: &marlin_front_hir_def::ItemTree, item: ItemId) -> ItemId {
    ItemId {
        file: item.file,
        local: tree.items[item.local].parent.unwrap_or(item.local),
    }
}

impl FnCx<'_, '_, '_> {
    /// The arguments for `interface`'s generic parameters that the type `on` implements it with
    /// (`DefaultMaterial : ISurfaceMaterial<DefaultMaterial>`); `None` if it does not, directly.
    pub(crate) fn interface_args(&mut self, interface: ItemId, on: &Ty) -> R<Option<Subst>> {
        let Ty::Adt { item: t, args } = on else {
            return Ok(None);
        };
        let impl_subst = Subst::new().with_args(*t, args);
        let types = self.l.cx.get::<ItemTypesQuery>(t)?;
        for base in &types.bases {
            if let Ty::Adt {
                item,
                args: base_args,
            } = impl_subst.ty(base)
                && item == interface
            {
                return Ok(Some(Subst::new().with_args(interface, &base_args)));
            }
        }
        Ok(None)
    }
}

impl FnCx<'_, '_, '_> {
    /// An interface's constant `item` read through a type (`M.Blend`, the type `M` stands for in
    /// this instance): the constant of the same name the type declares, or else the interface's
    /// default, evaluated with the type's arguments for the interface.
    pub(crate) fn interface_const(&mut self, item: ItemId, e: ExprId) -> R<(Value, Ty)> {
        let loc = InferLoc::Expr(e);
        let tree = self.l.cx.get::<ItemTreeQuery>(&item.file)?;
        let name = tree.items[item.local].name.clone();
        let on = self.infer.receivers.get(e).map(|t| self.concrete(t));
        let Some(on) = on else {
            return Err(LowerError::unsupported(
                "an interface's constant without a concrete type",
                self.owner(),
                loc,
            ));
        };
        let Ty::Adt { item: t, args } = on.clone() else {
            return Err(LowerError::unsupported(
                "an interface's constant of a type that is not a struct",
                self.owner(),
                loc,
            ));
        };
        let ttree = self.l.cx.get::<ItemTreeQuery>(&t.file)?;
        let own = ttree.items[t.local].members.iter().find(|&&m| {
            let d = &ttree.items[m];
            d.is_const && d.kind == ItemKind::Field && d.name == name
        });
        let (value, ty) = match own {
            Some(&m) => {
                let member = ItemId {
                    file: t.file,
                    local: m,
                };
                let types = self.l.cx.get::<ItemTypesQuery>(&member)?;
                let ty = Subst::new()
                    .with_args(t, &args)
                    .ty(types.ty.as_ref().unwrap_or(&Ty::Error));
                let ty = self.concrete(&ty);
                let v = self
                    .l
                    .const_operand(member, &ty)
                    .map_err(|err| err.located(self.owner(), loc.clone()))?;
                (v, ty)
            }
            None => {
                let interface = item_parent(&tree, item);
                let subst = self.interface_args(interface, &on)?.ok_or_else(|| {
                    LowerError::unsupported(
                        "an interface's constant of a type that does not implement it",
                        self.owner(),
                        loc.clone(),
                    )
                })?;
                let types = self.l.cx.get::<ItemTypesQuery>(&item)?;
                let ty = self.concrete(&subst.ty(types.ty.as_ref().unwrap_or(&Ty::Error)));
                let v = self
                    .l
                    .const_value_in(item, &subst)
                    .map_err(|err| err.located(self.owner(), loc.clone()))?;
                let t = self.l.ty(&ty)?;
                let v = self
                    .l
                    .val_const(&v.val, t)
                    .map_err(|err| err.located(self.owner(), loc.clone()))?;
                (v, ty)
            }
        };
        let v = self.f.const_value(value, self.l.module.consts[value].ty);
        Ok((v, ty))
    }
}
