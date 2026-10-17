//! Member lookup on a type: fields, properties, methods, nested types, swizzles, `Length`, and
//! members of extension blocks and of the interfaces that bound a type parameter.

use marlin_front_hir_def::{
    FileImports, ItemId, ItemKind, ItemTreeQuery, Name, NamespacePath, PackageDefMap,
    PackageImports, VisiblePackages,
};
use marlin_lang_query::Cancelled;

use super::InferCtx;
use crate::item::ItemTypesQuery;
use crate::subst::Subst;
use crate::ty::{ConstTy, Ty};

/// One step of a value path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    Field(ItemId),
    Property(ItemId),
    /// Vector components by index (`xyzw` / `rgba` → 0..4).
    Swizzle(Vec<u8>),
    /// An array's length, a constant.
    Length,
}

/// What a member name finds.
#[derive(Clone, Debug)]
pub(crate) enum Member {
    Field {
        item: ItemId,
        ty: Ty,
    },
    Property {
        item: ItemId,
        ty: Ty,
    },
    /// Methods (an overload set), each with the substitution of its declaring type.
    Methods(Vec<(ItemId, Subst)>),
    Swizzle {
        components: Vec<u8>,
        ty: Ty,
    },
    Length,
    /// A nested or associated type.
    Type(Ty),
    /// Only inaccessible members have this name.
    Inaccessible,
    None,
}

/// The components a swizzle names, or `None` if `name` is not a swizzle.
fn swizzle(name: &str) -> Option<Vec<u8>> {
    if name.is_empty() || name.len() > 4 {
        return None;
    }
    let xyzw = name
        .chars()
        .map(|c| "xyzw".find(c))
        .collect::<Option<Vec<_>>>();
    let rgba = name
        .chars()
        .map(|c| "rgba".find(c))
        .collect::<Option<Vec<_>>>();
    xyzw.or(rgba)
        .map(|c| c.into_iter().map(|i| i as u8).collect())
}

impl InferCtx<'_, '_> {
    /// Looks `name` up on values of type `ty`. `statics` selects static members (through a
    /// type name) instead of instance members.
    pub(crate) fn lookup_member(
        &mut self,
        ty: &Ty,
        name: &Name,
        statics: bool,
    ) -> Result<Member, Cancelled> {
        if let Some((element, n)) = self.vector_of(ty)
            && !statics
            && let Some(components) = swizzle(name.as_str())
        {
            if let Some(len) = n.value()
                && components.iter().any(|&c| i128::from(c) >= len)
            {
                return Ok(Member::None);
            }
            let ty = if components.len() == 1 {
                element
            } else {
                self.vector_ty(element, ConstTy::Value(components.len() as i128))
            };
            return Ok(Member::Swizzle { components, ty });
        }
        if let Ty::Array { .. } = ty
            && !statics
            && name.as_str() == "Length"
        {
            return Ok(Member::Length);
        }

        // The declarations whose members apply: the type itself, or a parameter's bounds.
        let mut owners: Vec<(ItemId, Subst)> = Vec::new();
        match ty {
            Ty::Adt { item, args } => owners.push((*item, Subst::new().with_args(*item, args))),
            Ty::Param { owner, index } => {
                let types = self.cx.get::<ItemTypesQuery>(owner)?;
                if let Some(param) = types.generics.get(*index as usize) {
                    for bound in &param.bounds {
                        if let Ty::Adt { item, args } = bound {
                            owners.push((*item, Subst::new().with_args(*item, args)));
                        }
                    }
                }
            }
            _ => {}
        }
        let mut found = self.members_named(&owners, name, statics)?;
        if matches!(found, Member::None) && !statics {
            found = self.extension_member(ty, name)?;
        }
        Ok(found)
    }

    fn members_named(
        &mut self,
        owners: &[(ItemId, Subst)],
        name: &Name,
        statics: bool,
    ) -> Result<Member, Cancelled> {
        let mut methods = Vec::new();
        let mut hidden = false;
        for (owner, subst) in owners {
            let tree = self.cx.get::<ItemTreeQuery>(&owner.file)?;
            for &local in &tree.items[owner.local].members {
                let data = &tree.items[local];
                if data.name.as_ref() != Some(name) {
                    continue;
                }
                let item = ItemId {
                    file: owner.file,
                    local,
                };
                if !self.resolver.accessible(self.cx, item)? {
                    hidden = true;
                    continue;
                }
                // Constants and enum members are static; so are nested types.
                let is_static =
                    data.is_static || data.is_const || data.kind == ItemKind::EnumMember;
                let types = self.cx.get::<ItemTypesQuery>(&item)?;
                let ty = || subst.ty(types.ty.as_ref().unwrap_or(&Ty::Error));
                match data.kind {
                    ItemKind::Field | ItemKind::EnumMember if is_static == statics => {
                        return Ok(Member::Field { item, ty: ty() });
                    }
                    ItemKind::Property if is_static == statics => {
                        return Ok(Member::Property { item, ty: ty() });
                    }
                    ItemKind::Function if is_static == statics => {
                        methods.push((item, subst.clone()));
                    }
                    kind if kind.is_type() || kind == ItemKind::AssocType => {
                        let ty = match kind {
                            ItemKind::AssocType | ItemKind::TypeAlias => {
                                types.ty.clone().map_or(Ty::adt(item), |t| subst.ty(&t))
                            }
                            _ => Ty::adt(item),
                        };
                        return Ok(Member::Type(ty));
                    }
                    _ => {}
                }
            }
        }
        Ok(if !methods.is_empty() {
            Member::Methods(methods)
        } else if hidden {
            Member::Inaccessible
        } else {
            Member::None
        })
    }

    /// Members of extension blocks in scope whose receiver matches `ty` (0004 §8.4).
    fn extension_member(&mut self, ty: &Ty, name: &Name) -> Result<Member, Cancelled> {
        let mut namespaces: Vec<NamespacePath> = self.resolver.namespace().ancestors().collect();
        let imports = self.cx.get::<FileImports>(&self.owner.item.file)?;
        namespaces.extend(imports.namespaces.iter().cloned());
        let package = self
            .cx
            .input::<marlin_front_hir_def::FilePackage>(&self.owner.item.file)?;
        let package_imports = self.cx.get::<PackageImports>(&package)?;
        namespaces.extend(package_imports.namespaces.iter().cloned());

        let mut blocks = Vec::new();
        for &p in self.cx.get::<VisiblePackages>(&package)?.iter() {
            let map = self.cx.get::<PackageDefMap>(&p)?;
            for ns in &namespaces {
                if let Some(scope) = map.namespace(ns) {
                    blocks.extend(scope.extensions.iter().copied());
                }
            }
        }
        blocks.sort();
        blocks.dedup();
        let mut methods = Vec::new();
        for block in blocks {
            let types = self.cx.get::<ItemTypesQuery>(&block)?;
            let Some(receiver) = &types.receiver else {
                continue;
            };
            let mut subst = Subst::new().with_unknowns(block, types.generics.len());
            if !subst.unify(receiver, ty) || !subst.complete(block) || subst.ty(receiver) != *ty {
                continue;
            }
            match self.members_named(&[(block, subst.clone())], name, false)? {
                Member::Methods(found) => methods.extend(found),
                Member::None => {}
                other => return Ok(other),
            }
        }
        Ok(if methods.is_empty() {
            Member::None
        } else {
            Member::Methods(methods)
        })
    }
}
