//! Signatures: the types, attributes, type parameters and constraints of a declaration, lowered
//! to HIR. `hir_ty` gives them meaning.

use std::sync::Arc;

use marlin_front_syntax::SyntaxKind;
use marlin_lang_query::{Cancelled, Cx, Query};

use crate::body::item_node;
use crate::item_tree::ItemKind;
use crate::lower::{Lowerer, child, child_of_kind, is_expr, is_type, name_of};
use crate::store::{Attribute, ExprId, ExprStore, RefKind, StoreSourceMap, TypeRefId};
use crate::{ItemId, ItemTreeQuery, Name};

use SyntaxKind::*;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Signature {
    /// Types and expressions (constant arguments, defaults, attribute arguments) live here.
    pub store: ExprStore,
    pub attributes: Vec<Attribute>,
    pub generic_params: Vec<GenericParamSig>,
    pub where_clauses: Vec<WhereClauseSig>,
    pub params: Vec<ParamSig>,
    /// A function's or operator's return type, a field's, property's or indexer's type, a
    /// conversion's target, an associated type's `= T`.
    pub ty: Option<TypeRefId>,
    /// Interfaces implemented, an enum's underlying type, an associated type's bounds.
    pub bases: Vec<TypeRefId>,
    /// An extension block's receiver.
    pub receiver: Option<ParamSig>,
    /// An operator declaration's operator, as written.
    pub operator: Option<Box<str>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GenericParamSig {
    pub name: Name,
    /// The type of a constant parameter (`const int N`).
    pub const_ty: Option<TypeRefId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WhereClauseSig {
    pub target: TypeRefId,
    pub bounds: Vec<Bound>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bound {
    Type(TypeRefId),
    /// C#'s `struct` constraint.
    Struct,
    /// C#'s `class` constraint.
    Class,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ParamSig {
    pub name: Option<Name>,
    pub attributes: Vec<Attribute>,
    pub modifier: Option<RefKind>,
    pub is_this: bool,
    pub is_params: bool,
    pub ty: TypeRefId,
    pub default: Option<ExprId>,
}

/// A signature and where its parts are in the source; compared without the map.
pub struct SignatureWithSourceMap;
impl Query for SignatureWithSourceMap {
    type Key = ItemId;
    type Value = (Arc<Signature>, Arc<StoreSourceMap>);
    fn execute(
        cx: &Cx<'_>,
        item: &ItemId,
    ) -> Result<(Arc<Signature>, Arc<StoreSourceMap>), Cancelled> {
        let tree = cx.get::<ItemTreeQuery>(&item.file)?;
        let data = &tree.items[item.local];
        let node = item_node(cx, *item)?;
        let mut lower = Lowerer::new();
        let mut sig = Signature {
            attributes: lower.attributes(&node),
            ..Signature::default()
        };

        if let Some(list) = child_of_kind(&node, TYPE_PARAM_LIST) {
            for param in list.children().filter(|c| c.kind() == TYPE_PARAM) {
                let Some(name) = name_of(&param) else {
                    continue;
                };
                let const_ty = child(&param, is_type).map(|t| lower.ty(&t));
                sig.generic_params.push(GenericParamSig { name, const_ty });
            }
        }
        for clause in node.children().filter(|c| c.kind() == WHERE_CLAUSE) {
            let types: Vec<_> = clause.children().filter(|c| is_type(c.kind())).collect();
            let Some((target, rest)) = types.split_first() else {
                continue;
            };
            let target = lower.ty(target);
            let mut bounds: Vec<Bound> = rest.iter().map(|t| Bound::Type(lower.ty(t))).collect();
            for t in clause.child_tokens() {
                match t.kind() {
                    STRUCT_KW => bounds.push(Bound::Struct),
                    CLASS_KW => bounds.push(Bound::Class),
                    _ => {}
                }
            }
            sig.where_clauses.push(WhereClauseSig { target, bounds });
        }

        let params: Vec<ParamSig> = child_of_kind(&node, PARAM_LIST)
            .map(|list| {
                list.children()
                    .filter(|c| c.kind() == PARAM)
                    .map(|p| {
                        let modifiers: Vec<SyntaxKind> = child_of_kind(&p, MODIFIERS)
                            .map(|m| m.child_tokens().map(|t| t.kind()).collect())
                            .unwrap_or_default();
                        let modifier = modifiers.iter().find_map(|k| match k {
                            IN_KW => Some(RefKind::In),
                            OUT_KW => Some(RefKind::Out),
                            REF_KW => Some(RefKind::Ref),
                            _ => None,
                        });
                        ParamSig {
                            name: name_of(&p),
                            attributes: lower.attributes(&p),
                            modifier,
                            is_this: modifiers.contains(&THIS_KW),
                            is_params: modifiers.contains(&PARAMS_KW),
                            ty: lower.type_opt(child(&p, is_type)),
                            default: child(&p, is_expr).map(|e| lower.expr(&e)),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        if data.kind == ItemKind::Extension {
            sig.receiver = params.into_iter().next();
        } else {
            sig.params = params;
        }

        sig.ty = match data.kind {
            ItemKind::Function
            | ItemKind::Operator
            | ItemKind::Conversion
            | ItemKind::Property
            | ItemKind::Indexer
            | ItemKind::Field
            | ItemKind::AssocType => child(&node, is_type).map(|t| lower.ty(&t)),
            _ => None,
        };
        if let Some(list) = child_of_kind(&node, BASE_LIST) {
            sig.bases = list
                .children()
                .filter(|c| is_type(c.kind()))
                .map(|t| lower.ty(&t))
                .collect();
        }
        if data.kind == ItemKind::Operator {
            sig.operator = node
                .child_tokens()
                .skip_while(|t| t.kind() != OPERATOR_KW)
                .skip(1)
                .find(|t| !t.kind().is_trivia())
                .map(|t| t.text().into());
        }
        sig.store = lower.store;
        Ok((Arc::new(sig), Arc::new(lower.map)))
    }
}

/// A signature without its source map.
pub struct ItemSignature;
impl Query for ItemSignature {
    type Key = ItemId;
    type Value = Arc<Signature>;
    fn execute(cx: &Cx<'_>, item: &ItemId) -> Result<Arc<Signature>, Cancelled> {
        Ok(cx.get::<SignatureWithSourceMap>(item)?.0)
    }
}
