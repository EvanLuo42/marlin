//! Bodies: the code of functions, accessors, and the initializers of fields, properties and enum
//! members, lowered to HIR.

use std::sync::Arc;

use marlin_front_syntax::{SyntaxKind, SyntaxNode};
use marlin_lang_query::{Cancelled, Cx, Query};

use crate::item_tree::ItemKind;
use crate::lower::{Lowerer, child, child_of_kind, is_expr, name_of};
use crate::store::{BindingId, BindingKind, ExprId, ExprStore, LambdaBody, StoreSourceMap};
use crate::{AstIdMap, Expr, ItemId, ItemTreeQuery, ParseFile};

use SyntaxKind::*;

/// Which code of a declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BodyPart {
    /// A function's body; a field's, enum member's or expression-bodied property's value.
    Main,
    /// The `n`th accessor of a property or indexer.
    Accessor(u8),
    /// A property's `= value` after its accessors.
    Initializer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BodyOwner {
    pub item: ItemId,
    pub part: BodyPart,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Body {
    pub store: ExprStore,
    /// The bindings in scope from the start: parameters, an extension receiver, a setter's
    /// `value`.
    pub params: Vec<BindingId>,
    pub root: Option<LambdaBody>,
    /// A constructor's `: this(...)` call.
    pub constructor_initializer: Option<ExprId>,
}

/// A body and where its parts are in the source. Bodies are compared without the map, so that
/// edits that only move text leave [`BodyQuery`] equal.
pub struct BodyWithSourceMap;
impl Query for BodyWithSourceMap {
    type Key = BodyOwner;
    type Value = (Arc<Body>, Arc<StoreSourceMap>);
    fn execute(
        cx: &Cx<'_>,
        owner: &BodyOwner,
    ) -> Result<(Arc<Body>, Arc<StoreSourceMap>), Cancelled> {
        let node = item_node(cx, owner.item)?;
        let tree = cx.get::<ItemTreeQuery>(&owner.item.file)?;
        let item = &tree.items[owner.item.local];
        let mut lower = Lowerer::new();
        let mut body = Body::default();

        // Bindings in scope from the start.
        if let Some(parent) = item.parent
            && tree.items[parent].kind == ItemKind::Extension
        {
            let extension = item_node(
                cx,
                ItemId {
                    file: owner.item.file,
                    local: parent,
                },
            )?;
            if let Some(receiver) = child_of_kind(&extension, PARAM_LIST)
                .and_then(|l| child_of_kind(&l, PARAM))
                .and_then(|p| child_of_kind(&p, NAME))
                && let Some(name) = receiver.first_token()
            {
                let name = crate::Name::new(name.text());
                body.params
                    .push(lower.binding(name, BindingKind::Receiver, Some(&receiver)));
            }
        }
        if let Some(list) = child_of_kind(&node, PARAM_LIST) {
            for (index, param) in list.children().filter(|c| c.kind() == PARAM).enumerate() {
                if let Some(name_node) = child_of_kind(&param, NAME) {
                    let name = name_of(&param).unwrap();
                    body.params.push(lower.binding(
                        name,
                        BindingKind::Param(index as u32),
                        Some(&name_node),
                    ));
                }
            }
        }

        match owner.part {
            BodyPart::Main => match item.kind {
                ItemKind::Field => {
                    let declarator = node
                        .children()
                        .filter(|c| c.kind() == VARIABLE_DECLARATOR)
                        .nth(item.declarator as usize);
                    body.root = declarator
                        .and_then(|d| child(&d, is_expr))
                        .map(|e| LambdaBody::Expr(lower.expr(&e)));
                }
                ItemKind::EnumMember => {
                    body.root = child(&node, is_expr).map(|e| LambdaBody::Expr(lower.expr(&e)));
                }
                _ => {
                    body.root = lower.function_body(&node);
                    if let Some(init) = child_of_kind(&node, CONSTRUCTOR_INITIALIZER) {
                        let callee = if init.child_tokens().any(|t| t.kind() == BASE_KW) {
                            Expr::Base
                        } else {
                            Expr::This
                        };
                        let callee = lower.store.exprs.alloc(callee);
                        let args = child_of_kind(&init, ARG_LIST)
                            .map(|l| lower.args(&l))
                            .unwrap_or_default();
                        body.constructor_initializer =
                            Some(lower.store.exprs.alloc(Expr::Call { callee, args }));
                    }
                }
            },
            BodyPart::Accessor(index) => {
                let accessor = child_of_kind(&node, ACCESSOR_LIST).and_then(|l| {
                    l.children()
                        .filter(|c| c.kind() == ACCESSOR)
                        .nth(index as usize)
                });
                if let Some(accessor) = accessor {
                    let is_setter = accessor
                        .child_tokens()
                        .any(|t| matches!(t.kind(), SET_KW | INIT_KW));
                    if is_setter {
                        body.params.push(lower.binding(
                            crate::Name::new("value"),
                            BindingKind::SetterValue,
                            None,
                        ));
                    }
                    body.root = lower.function_body(&accessor);
                }
            }
            BodyPart::Initializer => {
                body.root = child(&node, is_expr).map(|e| LambdaBody::Expr(lower.expr(&e)));
            }
        }
        body.store = lower.store;
        Ok((Arc::new(body), Arc::new(lower.map)))
    }
}

/// A body without its source map.
pub struct BodyQuery;
impl Query for BodyQuery {
    type Key = BodyOwner;
    type Value = Arc<Body>;
    fn execute(cx: &Cx<'_>, owner: &BodyOwner) -> Result<Arc<Body>, Cancelled> {
        Ok(cx.get::<BodyWithSourceMap>(owner)?.0)
    }
}

/// The syntax node of a declaration in its file's current tree.
pub(crate) fn item_node(cx: &Cx<'_>, item: ItemId) -> Result<SyntaxNode, Cancelled> {
    let tree = cx.get::<ItemTreeQuery>(&item.file)?;
    let ids = cx.get::<AstIdMap>(&item.file)?;
    let parse = cx.get::<ParseFile>(&item.file)?;
    Ok(ids
        .ptr(tree.items[item.local].ast)
        .to_node(&parse.syntax_node()))
}

/// The bodies a declaration has, in source order.
pub fn body_owners(cx: &Cx<'_>, item: ItemId) -> Result<Vec<BodyOwner>, Cancelled> {
    let tree = cx.get::<ItemTreeQuery>(&item.file)?;
    let data = &tree.items[item.local];
    let node = item_node(cx, item)?;
    let owner = |part| BodyOwner { item, part };
    let has_body =
        |n: &SyntaxNode| child_of_kind(n, BLOCK).is_some() || child_of_kind(n, EXPR_BODY).is_some();
    let mut owners = Vec::new();
    match data.kind {
        ItemKind::Function | ItemKind::Constructor | ItemKind::Operator | ItemKind::Conversion => {
            if has_body(&node) {
                owners.push(owner(BodyPart::Main));
            }
        }
        ItemKind::Field => {
            let declarator = node
                .children()
                .filter(|c| c.kind() == VARIABLE_DECLARATOR)
                .nth(data.declarator as usize);
            if declarator.is_some_and(|d| child(&d, is_expr).is_some()) {
                owners.push(owner(BodyPart::Main));
            }
        }
        ItemKind::EnumMember => {
            if child(&node, is_expr).is_some() {
                owners.push(owner(BodyPart::Main));
            }
        }
        ItemKind::Property | ItemKind::Indexer => {
            if child_of_kind(&node, EXPR_BODY).is_some() {
                owners.push(owner(BodyPart::Main));
            }
            if let Some(list) = child_of_kind(&node, ACCESSOR_LIST) {
                for (i, accessor) in list.children().filter(|c| c.kind() == ACCESSOR).enumerate() {
                    if has_body(&accessor) {
                        owners.push(owner(BodyPart::Accessor(i as u8)));
                    }
                }
            }
            if child(&node, is_expr).is_some() {
                owners.push(owner(BodyPart::Initializer));
            }
        }
        _ => {}
    }
    Ok(owners)
}
