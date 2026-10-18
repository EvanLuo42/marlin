//! The types of a declaration's signature.

use std::collections::HashSet;
use std::sync::Arc;

use marlin_front_hir_def::Bound;
use marlin_front_hir_def::store::{RefKind, TypeRef};
use marlin_front_hir_def::{
    FilePackage, ItemId, ItemKind, ItemSignature, ItemTreeQuery, Name, Resolution, Resolver,
    body_owners,
};
use marlin_lang_query::{Cancelled, Cx, Cycle, Query};

use crate::builtins::{BuiltinKind, BuiltinsQuery, resolve_attribute, string_arg};
use crate::diagnostics::{TyDiagnostic, TyDiagnosticKind, TyLoc};
use crate::lower::TyLowerer;
use crate::ty::{ConstTy, GenericArgTy, Ty};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GenericParamTy {
    pub name: Name,
    /// For a constant parameter, its type (`int` or `uint`).
    pub const_ty: Option<Ty>,
    /// Interfaces from `where` clauses.
    pub bounds: Vec<Ty>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ParamTy {
    pub name: Option<Name>,
    pub ref_kind: Option<RefKind>,
    pub ty: Ty,
    pub has_default: bool,
}

/// Everything a signature says, as types.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ItemTypes {
    pub generics: Vec<GenericParamTy>,
    /// `where M.Params : IFoo`: constraints on associated types.
    pub assoc_bounds: Vec<(Ty, Vec<Ty>)>,
    pub params: Vec<ParamTy>,
    /// A function's or operator's return type; a constructor's struct; a field's, property's,
    /// indexer's or enum member's type; a conversion's target; an alias's target.
    pub ty: Option<Ty>,
    /// An extension block's receiver.
    pub receiver: Option<Ty>,
    /// Implemented interfaces, or an enum's underlying type.
    pub bases: Vec<Ty>,
    /// `[Builtin("name")]`
    pub builtin: Option<BuiltinKind>,
    /// `[Intrinsic("op")]`
    pub intrinsic: Option<Box<str>>,
    pub diagnostics: Vec<TyDiagnostic>,
}

/// The type a type with generic parameters has inside its own declaration: its parameters as
/// arguments.
pub fn self_ty(cx: &Cx<'_>, item: ItemId) -> Result<Ty, Cancelled> {
    let tree = cx.get::<ItemTreeQuery>(&item.file)?;
    let args: Vec<GenericArgTy> = tree.items[item.local]
        .generic_params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (owner, index) = (item, i as u32);
            if p.is_const {
                GenericArgTy::Const(ConstTy::Param { owner, index })
            } else {
                GenericArgTy::Type(Ty::Param { owner, index })
            }
        })
        .collect();
    Ok(Ty::Adt {
        item,
        args: args.into(),
    })
}

pub struct ItemTypesQuery;
impl Query for ItemTypesQuery {
    type Key = ItemId;
    type Value = Arc<ItemTypes>;
    fn execute(cx: &Cx<'_>, item: &ItemId) -> Result<Arc<ItemTypes>, Cancelled> {
        let item = *item;
        let tree = cx.get::<ItemTreeQuery>(&item.file)?;
        let data = &tree.items[item.local];
        let sig = cx.get::<ItemSignature>(&item)?;
        let package = cx.input::<FilePackage>(&item.file)?;
        let builtins = cx.get::<BuiltinsQuery>(&package)?;
        let mut lower = TyLowerer {
            cx,
            owner: item,
            store: &sig.store,
            resolver: Resolver::for_item(cx, item)?,
            builtins: builtins.clone(),
            diagnostics: Vec::new(),
            in_attribute: in_attribute_declaration(cx, item, &builtins)?,
        };
        let mut out = ItemTypes::default();

        for param in &sig.generic_params {
            let const_ty = match param.const_ty {
                Some(t) => {
                    let ty = lower.ty(t)?;
                    let scalar = match &ty {
                        Ty::Adt { item, .. } => builtins.scalar(*item),
                        _ => None,
                    };
                    if !ty.is_error()
                        && !matches!(scalar, Some(crate::Scalar::I32 | crate::Scalar::U32))
                    {
                        let shown = crate::display_ty(cx, &builtins, &ty)?;
                        lower.diagnostics.push(TyDiagnostic {
                            item,
                            loc: TyLoc::Type(t),
                            kind: TyDiagnosticKind::BadConstParamType { ty: shown },
                        });
                    }
                    Some(ty)
                }
                None => None,
            };
            out.generics.push(GenericParamTy {
                name: param.name.clone(),
                const_ty,
                bounds: Vec::new(),
            });
        }
        for clause in &sig.where_clauses {
            let target = lower.ty(clause.target)?;
            let mut bounds = Vec::new();
            for bound in &clause.bounds {
                if let Bound::Type(t) = bound {
                    let ty = lower.ty(*t)?;
                    check_interface(&mut lower, &ty, *t)?;
                    bounds.push(ty);
                }
            }
            match target {
                Ty::Param { owner, index } if owner == item => {
                    out.generics[index as usize].bounds.extend(bounds);
                }
                target => out.assoc_bounds.push((target, bounds)),
            }
        }

        for param in &sig.params {
            out.params.push(ParamTy {
                name: param.name.clone(),
                ref_kind: param.modifier,
                ty: lower.ty(param.ty)?,
                has_default: param.default.is_some(),
            });
        }
        if let Some(receiver) = &sig.receiver {
            out.receiver = Some(lower.ty(receiver.ty)?);
        }
        out.ty = match data.kind {
            ItemKind::Constructor => match data.parent {
                Some(parent) => Some(self_ty(
                    cx,
                    ItemId {
                        file: item.file,
                        local: parent,
                    },
                )?),
                None => Some(Ty::Error),
            },
            ItemKind::EnumMember => data.parent.map(|parent| {
                Ty::adt(ItemId {
                    file: item.file,
                    local: parent,
                })
            }),
            ItemKind::TypeAlias | ItemKind::AssocType => {
                if alias_cycle(cx, item)? {
                    lower.diagnostics.push(TyDiagnostic {
                        item,
                        loc: TyLoc::Item,
                        kind: TyDiagnosticKind::AliasCycle,
                    });
                    Some(Ty::Error)
                } else {
                    match sig.ty {
                        Some(t) => Some(lower.ty(t)?),
                        None => None,
                    }
                }
            }
            _ => match sig.ty {
                Some(t) => Some(lower.ty(t)?),
                None => None,
            },
        };
        for &base in &sig.bases {
            let ty = lower.ty(base)?;
            if data.kind != ItemKind::Enum {
                check_interface(&mut lower, &ty, base)?;
            }
            out.bases.push(ty);
        }

        // [Builtin] and [Intrinsic].
        for attribute in &sig.attributes {
            let path = attribute.path.to_def_path();
            let Some(attr) = resolve_attribute(cx, &lower.resolver, &path)? else {
                continue;
            };
            let name = attribute
                .args
                .first()
                .and_then(|a| string_arg(&sig.store, a.expr));
            let which = if Some(attr) == builtins.builtin_attribute {
                out.builtin = builtins.kind(item);
                "Builtin"
            } else if Some(attr) == builtins.intrinsic_attribute {
                out.intrinsic = name.map(Into::into);
                "Intrinsic"
            } else {
                continue;
            };
            if builtins.core_package != Some(package) {
                lower.diagnostics.push(TyDiagnostic {
                    item,
                    loc: TyLoc::Item,
                    kind: TyDiagnosticKind::AttributeOutsideCore { attribute: which },
                });
            }
        }

        let bodiless = matches!(
            data.kind,
            ItemKind::Function | ItemKind::Operator | ItemKind::Conversion
        ) && body_owners(cx, item)?.is_empty();
        let in_interface = data
            .parent
            .is_some_and(|p| tree.items[p].kind == ItemKind::Interface);
        if bodiless && out.intrinsic.is_none() && !in_interface {
            lower.diagnostics.push(TyDiagnostic {
                item,
                loc: TyLoc::Item,
                kind: TyDiagnosticKind::MissingBody,
            });
        }

        out.diagnostics = lower.diagnostics;
        Ok(Arc::new(out))
    }

    /// Only an alias cycle the check above misses could get here; give the reader an error type.
    fn cycle_fallback(_: &Cycle, item: &ItemId) -> Option<Arc<ItemTypes>> {
        Some(Arc::new(ItemTypes {
            ty: Some(Ty::Error),
            diagnostics: vec![TyDiagnostic {
                item: *item,
                loc: TyLoc::Item,
                kind: TyDiagnosticKind::AliasCycle,
            }],
            ..ItemTypes::default()
        }))
    }
}

fn check_interface(
    lower: &mut TyLowerer<'_, '_>,
    ty: &Ty,
    t: marlin_front_hir_def::store::TypeRefId,
) -> Result<(), Cancelled> {
    let Ty::Adt { item, .. } = ty else {
        return Ok(());
    };
    let tree = lower.cx.get::<ItemTreeQuery>(&item.file)?;
    if tree.items[item.local].kind != ItemKind::Interface {
        let shown = crate::display_ty(lower.cx, &lower.builtins, ty)?;
        lower.diagnostics.push(TyDiagnostic {
            item: lower.owner,
            loc: TyLoc::Type(t),
            kind: TyDiagnosticKind::NotAnInterface { ty: shown },
        });
    }
    Ok(())
}

/// Whether the types an alias mentions (its target, the target's type arguments, and so on
/// through other aliases) come back to it.
fn alias_cycle(cx: &Cx<'_>, start: ItemId) -> Result<bool, Cancelled> {
    let mut seen = HashSet::new();
    let mut stack = vec![start];
    while let Some(alias) = stack.pop() {
        if !seen.insert(alias) {
            continue;
        }
        let sig = cx.get::<ItemSignature>(&alias)?;
        // An interface's associated type has no target, only bounds, and a bound may name the
        // type itself (`type Tree : ISubstrateNode<Tree>;`): nothing to follow.
        if sig.ty.is_none() {
            continue;
        }
        let resolver = Resolver::for_item(cx, alias)?;
        for (_, ty) in sig.store.types.iter() {
            let TypeRef::Path(path) = ty else { continue };
            let Resolution::Type(next) = resolver.resolve(cx, &path.to_def_path(), false)? else {
                continue;
            };
            let tree = cx.get::<ItemTreeQuery>(&next.file)?;
            if matches!(
                tree.items[next.local].kind,
                ItemKind::TypeAlias | ItemKind::AssocType
            ) {
                if next == start {
                    return Ok(true);
                }
                stack.push(next);
            }
        }
    }
    Ok(false)
}

/// What an alias or associated type stands for; `None` for an interface's associated type,
/// which has no `= T`.
pub struct AliasTargetQuery;
impl Query for AliasTargetQuery {
    type Key = ItemId;
    type Value = Option<Ty>;
    fn execute(cx: &Cx<'_>, item: &ItemId) -> Result<Option<Ty>, Cancelled> {
        // No target, without lowering the item: an interface's associated type may be named by
        // its own bounds (`type Tree : ISubstrateNode<Tree>;`), which lowering it would cycle on.
        if cx.get::<ItemSignature>(item)?.ty.is_none() {
            return Ok(None);
        }
        Ok(cx.get::<ItemTypesQuery>(item)?.ty.clone())
    }
    fn cycle_fallback(_: &Cycle, _: &ItemId) -> Option<Option<Ty>> {
        // Reported as E1007 on the alias itself.
        Some(Some(Ty::Error))
    }
}

/// Whether `item` is, or is a member of, a struct that implements `Marlin.IAttribute`.
fn in_attribute_declaration(
    cx: &Cx<'_>,
    item: ItemId,
    builtins: &crate::Builtins,
) -> Result<bool, Cancelled> {
    let Some(interface) = builtins.attribute_interface else {
        return Ok(false);
    };
    let tree = cx.get::<ItemTreeQuery>(&item.file)?;
    let data = &tree.items[item.local];
    let owner = if data.kind == ItemKind::Struct {
        item
    } else {
        match data.parent {
            Some(p) if tree.items[p].kind == ItemKind::Struct => ItemId {
                file: item.file,
                local: p,
            },
            _ => return Ok(false),
        }
    };
    let sig = cx.get::<ItemSignature>(&owner)?;
    let resolver = Resolver::for_item(cx, owner)?;
    for &base in &sig.bases {
        if let TypeRef::Path(path) = &sig.store.types[base]
            && resolver.resolve(cx, &path.to_def_path(), false)? == Resolution::Type(interface)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `ty` with each associated type of a concrete type (`HasFloat.Item`, after `M = HasFloat`)
/// replaced by what that type declares: an alias's target, or a nested type.
pub fn normalize_assoc(cx: &Cx<'_>, ty: &Ty) -> Result<Ty, Cancelled> {
    Ok(match ty {
        Ty::Assoc { base, name } => {
            let base = normalize_assoc(cx, base)?;
            let Ty::Adt { item, args } = &base else {
                return Ok(Ty::Assoc {
                    base: base.into(),
                    name: name.clone(),
                });
            };
            let tree = cx.get::<ItemTreeQuery>(&item.file)?;
            for &m in &tree.items[item.local].members {
                let d = &tree.items[m];
                if d.name.as_ref() != Some(name) {
                    continue;
                }
                let member = ItemId {
                    file: item.file,
                    local: m,
                };
                if matches!(d.kind, ItemKind::AssocType | ItemKind::TypeAlias) {
                    if let Some(target) = cx.get::<AliasTargetQuery>(&member)? {
                        let target = crate::Subst::new().with_args(*item, args).ty(&target);
                        return normalize_assoc(cx, &target);
                    }
                } else if d.kind.is_type() {
                    return Ok(Ty::adt(member));
                }
            }
            Ty::Assoc {
                base: base.into(),
                name: name.clone(),
            }
        }
        Ty::Adt { item, args } => Ty::Adt {
            item: *item,
            args: args
                .iter()
                .map(|a| match a {
                    GenericArgTy::Type(t) => normalize_assoc(cx, t).map(GenericArgTy::Type),
                    c => Ok(c.clone()),
                })
                .collect::<Result<_, _>>()?,
        },
        Ty::Array { element, len } => Ty::Array {
            element: normalize_assoc(cx, element)?.into(),
            len: len.clone(),
        },
        Ty::Tuple(items) => Ty::Tuple(
            items
                .iter()
                .map(|t| normalize_assoc(cx, t))
                .collect::<Result<_, _>>()?,
        ),
        _ => ty.clone(),
    })
}

/// The interfaces `bounds` name and every interface they extend, through their bases, each
/// once with its arguments: where the members of a type parameter come from.
pub(crate) fn bound_interfaces(
    cx: &Cx<'_>,
    bounds: &[Ty],
) -> Result<Vec<(ItemId, crate::Subst)>, Cancelled> {
    let mut out: Vec<(ItemId, crate::Subst)> = Vec::new();
    let mut seen: HashSet<Ty> = HashSet::new();
    let mut work: Vec<Ty> = bounds.iter().rev().cloned().collect();
    while let Some(t) = work.pop() {
        let Ty::Adt { item, args } = &t else { continue };
        if !seen.insert(t.clone()) {
            continue;
        }
        let subst = crate::Subst::new().with_args(*item, args);
        let types = cx.get::<ItemTypesQuery>(item)?;
        work.extend(types.bases.iter().rev().map(|b| subst.ty(b)));
        out.push((*item, subst));
    }
    Ok(out)
}
