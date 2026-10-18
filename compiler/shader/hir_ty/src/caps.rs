//! Capability inference (0004 §12.3): which optional GPU features a function needs.
//!
//! The sources are the types a function uses (`half`, `short`, `ushort` need `Native16Bit`;
//! `long`, `ulong` need `Int64`; `double` needs `Float64`), types and functions marked
//! `[Requires(...)]`, and, through the call graph, what the functions it calls need. A
//! function with a `[Requires]` contract stands for what it declares. What is used only
//! inside a `const if` branch is kept apart: a target may prune that branch, which is decided
//! against the target description (0005 §11 step 9), not here.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use marlin_front_hir_def::store::{Expr, ExprStore, Node, Pat, Stmt};
use marlin_front_hir_def::{
    BodyQuery, FilePackage, ItemId, ItemSignature, ItemTreeQuery, body_owners,
};
use marlin_lang_query::{Cancelled, Cx, Cycle, Query};

use crate::attrs::{ItemAttributesQuery, KnownAttr};
use crate::builtins::{BuiltinKind, Builtins, BuiltinsQuery, Scalar};
use crate::infer::{Callee, InferQuery};
use crate::item::ItemTypesQuery;
use crate::ty::{GenericArgTy, Ty};

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Capabilities {
    /// Each capability needed, with why (the first reason found), as "uses `half`".
    pub required: BTreeMap<Box<str>, String>,
    /// Needed only inside `const if` branches.
    pub conditional: BTreeMap<Box<str>, String>,
}

/// The capabilities `[Requires(...)]` on `item` declares; `None` without the attribute.
pub fn declared_requirements(
    cx: &Cx<'_>,
    item: ItemId,
) -> Result<Option<BTreeSet<Box<str>>>, Cancelled> {
    let attrs = cx.get::<ItemAttributesQuery>(&item)?;
    let mut found = false;
    let mut out = BTreeSet::new();
    let sig = cx.get::<ItemSignature>(&item)?;
    for attr in attrs
        .item
        .iter()
        .filter(|a| a.known == Some(KnownAttr::Requires))
    {
        found = true;
        for (_, arg) in &attr.args {
            if let Some(name) = capability_name(&sig.store, *arg) {
                out.insert(name);
            }
        }
    }
    Ok(found.then_some(out))
}

/// `Capability.WaveMatrix` (or `WaveMatrix`) → `WaveMatrix`.
fn capability_name(store: &ExprStore, e: marlin_front_hir_def::store::ExprId) -> Option<Box<str>> {
    match &store.exprs[e] {
        Expr::Path(path) => path.segments.last().map(|s| s.name.as_str().into()),
        Expr::Member { name, .. } => Some(name.as_str().into()),
        _ => None,
    }
}

fn scalar_capability(s: Scalar) -> Option<&'static str> {
    match s {
        Scalar::F16 | Scalar::I16 | Scalar::U16 => Some("Native16Bit"),
        Scalar::I64 | Scalar::U64 => Some("Int64"),
        Scalar::F64 => Some("Float64"),
        _ => None,
    }
}

struct Collector<'a, 'b> {
    cx: &'a Cx<'b>,
    builtins: Arc<Builtins>,
    out: Capabilities,
    /// Types already walked, outside and inside `const if` branches.
    seen: [HashSet<Ty>; 2],
}

impl Collector<'_, '_> {
    fn add(&mut self, conditional: bool, cap: &str, why: impl FnOnce() -> String) {
        let map = if conditional {
            &mut self.out.conditional
        } else {
            &mut self.out.required
        };
        map.entry(cap.into()).or_insert_with(why);
    }

    fn ty(&mut self, ty: &Ty, conditional: bool) -> Result<(), Cancelled> {
        if !self.seen[conditional as usize].insert(ty.clone()) {
            return Ok(());
        }
        match ty {
            Ty::Adt { item, args } => {
                if let Some(BuiltinKind::Scalar(s)) = self.builtins.kind(*item)
                    && let Some(cap) = scalar_capability(s)
                {
                    self.add(conditional, cap, || format!("uses `{}`", s.keyword()));
                }
                if let Some(caps) = declared_requirements(self.cx, *item)? {
                    let name = item_name(self.cx, *item)?;
                    for cap in caps {
                        self.add(conditional, &cap, || format!("uses `{name}`"));
                    }
                }
                for arg in args.iter() {
                    if let GenericArgTy::Type(t) = arg {
                        self.ty(t, conditional)?;
                    }
                }
            }
            Ty::Array { element, .. } => self.ty(element, conditional)?,
            Ty::Tuple(items) => {
                for t in items.iter() {
                    self.ty(t, conditional)?;
                }
            }
            Ty::Assoc { base, .. } => self.ty(base, conditional)?,
            Ty::Error | Ty::Void | Ty::Param { .. } => {}
        }
        Ok(())
    }

    fn call(&mut self, callee: ItemId, conditional: bool) -> Result<(), Cancelled> {
        let name = item_name(self.cx, callee)?;
        let (required, inner_conditional) = match declared_requirements(self.cx, callee)? {
            Some(declared) => (declared.into_iter().collect::<Vec<_>>(), Vec::new()),
            None => {
                let caps = self.cx.get::<CapabilitiesQuery>(&callee)?;
                (
                    caps.required.keys().cloned().collect(),
                    caps.conditional.keys().cloned().collect(),
                )
            }
        };
        for cap in required {
            self.add(conditional, &cap, || format!("calls `{name}`"));
        }
        for cap in inner_conditional {
            self.add(true, &cap, || format!("calls `{name}`"));
        }
        Ok(())
    }

    /// A 64-bit atomic (`ty` is what it gives, its element's type) needs `Int64Atomics` on
    /// buffers and groupshared memory, `Int64TextureAtomics` on textures (2026-10-18).
    fn atomic(&mut self, callee: ItemId, ty: &Ty, conditional: bool) -> Result<(), Cancelled> {
        let types = self.cx.get::<ItemTypesQuery>(&callee)?;
        let Some(intrinsic) = types.intrinsic.as_deref() else {
            return Ok(());
        };
        let cap = if intrinsic.starts_with("atomic.") {
            "Int64Atomics"
        } else if intrinsic.starts_with("tex.atomic.") {
            "Int64TextureAtomics"
        } else {
            return Ok(());
        };
        let wide = matches!(ty, Ty::Adt { item, .. }
            if matches!(self.builtins.kind(*item), Some(BuiltinKind::Scalar(Scalar::I64 | Scalar::U64))));
        if wide {
            let name = item_name(self.cx, callee)?;
            self.add(conditional, cap, || {
                format!("calls `{name}` on 64-bit integers")
            });
        }
        Ok(())
    }
}

fn item_name(cx: &Cx<'_>, item: ItemId) -> Result<String, Cancelled> {
    let tree = cx.get::<ItemTreeQuery>(&item.file)?;
    Ok(tree.items[item.local]
        .name
        .as_ref()
        .map_or_else(|| "operator".to_string(), |n| n.to_string()))
}

/// The nodes inside `const if` branches of a body.
fn conditional_nodes(store: &ExprStore) -> HashSet<Node> {
    let mut out = HashSet::new();
    for (_, stmt) in store.stmts.iter() {
        if let Stmt::If {
            is_const: true,
            then,
            otherwise,
            ..
        } = stmt
        {
            for branch in std::iter::once(*then).chain(*otherwise) {
                out.extend(store.descendants(Node::Stmt(branch)));
            }
        }
    }
    out
}

/// What a function (or any item with bodies) needs.
pub struct CapabilitiesQuery;
impl Query for CapabilitiesQuery {
    type Key = ItemId;
    type Value = Arc<Capabilities>;
    fn execute(cx: &Cx<'_>, item: &ItemId) -> Result<Arc<Capabilities>, Cancelled> {
        let package = cx.input::<FilePackage>(&item.file)?;
        let mut c = Collector {
            cx,
            builtins: cx.get::<BuiltinsQuery>(&package)?,
            out: Capabilities::default(),
            seen: Default::default(),
        };
        let types = cx.get::<ItemTypesQuery>(item)?;
        for p in &types.params {
            c.ty(&p.ty, false)?;
        }
        if let Some(t) = &types.ty {
            c.ty(t, false)?;
        }
        for owner in body_owners(cx, *item)? {
            let body = cx.get::<BodyQuery>(&owner)?;
            let infer = cx.get::<InferQuery>(&owner)?;
            let conditional = conditional_nodes(&body.store);
            let in_branch = |e| conditional.contains(&Node::Expr(e));
            // Bindings declared inside a branch.
            let mut branch_bindings = HashSet::new();
            for node in &conditional {
                match node {
                    Node::Stmt(s) => {
                        if let Stmt::Local { declarators, .. } = &body.store.stmts[*s] {
                            branch_bindings.extend(declarators.iter().map(|(b, _)| *b));
                        }
                    }
                    Node::Expr(e) => match &body.store.exprs[*e] {
                        Expr::DeclVar { binding, .. } => {
                            branch_bindings.insert(*binding);
                        }
                        Expr::DeclTuple(bindings) => {
                            branch_bindings.extend(bindings.iter().copied())
                        }
                        _ => {}
                    },
                    Node::Pat(p) => {
                        if let Pat::Var(b) = &body.store.pats[*p] {
                            branch_bindings.insert(*b);
                        }
                    }
                }
            }
            for (b, ty) in infer.binding_types.iter() {
                c.ty(ty, branch_bindings.contains(&b))?;
            }
            for (e, ty) in infer.expr_types.iter() {
                c.ty(ty, in_branch(e))?;
            }
            for (e, callee) in infer.callees.iter() {
                if let Callee::Item { item: callee, .. } | Callee::ForwardDiff { item: callee, .. } =
                    callee
                    && callee != item
                {
                    c.call(*callee, in_branch(e))?;
                    if let Some(ty) = infer.expr_types.get(e) {
                        c.atomic(*callee, ty, in_branch(e))?;
                    }
                }
            }
        }
        // What is required anyway is not conditional.
        let required: Vec<Box<str>> = c.out.required.keys().cloned().collect();
        for cap in required {
            c.out.conditional.remove(&cap);
        }
        Ok(Arc::new(c.out))
    }

    /// Recursion only terminates at compile time (0004 §6); a cycle adds nothing of its own.
    fn cycle_fallback(_: &Cycle, _: &ItemId) -> Option<Arc<Capabilities>> {
        Some(Arc::new(Capabilities::default()))
    }
}
