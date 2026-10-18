//! `where T : I<...>` bounds at a call: the chosen overload's type arguments must implement
//! the interfaces its generic parameters are bounded by (0004 §8.6).

use marlin_front_hir_def::{ItemId, ItemTreeQuery, item_path};
use marlin_lang_query::Cancelled;

use super::{InferCtx, InferLoc};
use crate::item::{ItemTypesQuery, bound_interfaces, normalize_assoc};
use crate::subst::Subst;
use crate::ty::{GenericArgTy, Ty};

impl InferCtx<'_, '_> {
    /// Reports each bound of `item`'s generic parameters that its arguments in `subst` do not
    /// meet.
    pub(crate) fn check_bounds(
        &mut self,
        at: InferLoc,
        item: ItemId,
        subst: &Subst,
    ) -> Result<(), Cancelled> {
        let types = self.cx.get::<ItemTypesQuery>(&item)?;
        for (index, param) in types.generics.iter().enumerate() {
            let Some(GenericArgTy::Type(arg)) = subst.get(item, index as u32).cloned() else {
                continue;
            };
            for bound in &param.bounds {
                let bound = normalize_assoc(self.cx, &subst.ty(bound))?;
                if self.implements(&arg, &bound)? {
                    continue;
                }
                let (shown, interface) = (self.show(&arg), self.show(&bound));
                let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
                let callee = match &tree.items[item.local].name {
                    Some(name) => name.to_string(),
                    None => item_path(self.cx, item)?,
                };
                self.report(
                    at.clone(),
                    "E1136",
                    format!(
                        "`{shown}` does not implement `{interface}`, which `{callee}` requires \
                         of `{}`",
                        param.name
                    ),
                );
            }
        }
        Ok(())
    }

    /// Whether `ty` implements the interface `bound`: a type through the interfaces it lists
    /// and their bases, a type parameter through its own bounds. A vector lists the numeric
    /// interfaces of itself but implements them per component, so it needs its element type
    /// to implement them too.
    fn implements(&self, ty: &Ty, bound: &Ty) -> Result<bool, Cancelled> {
        let Ty::Adt {
            item: interface,
            args,
        } = bound
        else {
            return Ok(true);
        };
        let listed: Vec<Ty> = match ty {
            Ty::Adt { item, args } => {
                let subst = Subst::new().with_args(*item, args);
                let types = self.cx.get::<ItemTypesQuery>(item)?;
                types.bases.iter().map(|b| subst.ty(b)).collect()
            }
            Ty::Param { owner, index } => {
                let types = self.cx.get::<ItemTypesQuery>(owner)?;
                match types.generics.get(*index as usize) {
                    Some(param) => param.bounds.clone(),
                    None => return Ok(true),
                }
            }
            // Associated types are bounded through their parameter's `where` clauses, which
            // member lookup does not follow yet either; nothing to hold them to.
            Ty::Error | Ty::Assoc { .. } => return Ok(true),
            Ty::Void | Ty::Array { .. } | Ty::Tuple(_) => return Ok(false),
        };
        let found = bound_interfaces(self.cx, &listed)?
            .iter()
            .any(|(i, s)| *i == *interface && s.args_of(*i)[..] == args[..]);
        if !found {
            return Ok(false);
        }
        match self.vector_of(ty) {
            Some((element, _)) => {
                let per_component = Ty::Adt {
                    item: *interface,
                    args: args
                        .iter()
                        .map(|a| match a {
                            GenericArgTy::Type(t) if t == ty => GenericArgTy::Type(element.clone()),
                            a => a.clone(),
                        })
                        .collect(),
                };
                self.implements(&element, &per_component)
            }
            None => Ok(true),
        }
    }
}
