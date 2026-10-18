//! Interface members called on concrete types (0004 §8.5: no dynamic dispatch): with the type
//! known in each instance, a call to `IShape.Area` on `S` is a call to the member of `S` that
//! implements it, and an interface operator on a scalar or vector is the built-in operator.

use marlin_front_hir_def::{ItemId, ItemKind, ItemSignature, ItemTreeQuery};
use marlin_shaderc_hir_ty::{InferLoc, ItemTypesQuery, Subst, Ty};

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
            .map(|c| c.0)
            .ok_or_else(|| {
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
