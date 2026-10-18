//! Checks of a typed body (step 5c): the rules that need the whole body and its types but do
//! not change them.
//!
//! - `flow`: definite assignment, `out` parameters, returns on every path, `switch` sections
//!   that fall through (0004 §6, §9.1);
//! - `rules`: writes to and non-`readonly` calls on read-only values (§5), exhaustive `switch`
//!   (§9.1);
//! - `diff`: what `[Differentiable]` functions call (§15.1).

mod decl;
mod diff;
mod flow;
mod rules;
pub(crate) mod types;

pub use decl::ItemChecksQuery;

use std::collections::BTreeSet;
use std::sync::Arc;

use marlin_front_hir_def::store::{Expr, Literal, Pat, PatId};
use marlin_front_hir_def::{
    Body, BodyOwner, BodyQuery, FilePackage, ItemId, ItemKind, ItemTreeQuery,
};
use marlin_lang_query::{Cancelled, Cx, Query};

use crate::attrs::{ItemAttributesQuery, KnownAttr};
use crate::builtins::{BuiltinKind, Builtins, BuiltinsQuery, Scalar};
use crate::infer::{InferDiagnostic, InferLoc, InferQuery, InferenceResult, Root};
use crate::ty::{ConstTy, GenericArgTy, Ty};

/// The diagnostics of the checks of one body.
pub struct BodyChecksQuery;
impl Query for BodyChecksQuery {
    type Key = BodyOwner;
    type Value = Arc<[InferDiagnostic]>;
    fn execute(cx: &Cx<'_>, owner: &BodyOwner) -> Result<Arc<[InferDiagnostic]>, Cancelled> {
        let package = cx.input::<FilePackage>(&owner.item.file)?;
        let mut checker = Checker {
            cx,
            owner: *owner,
            body: cx.get::<BodyQuery>(owner)?,
            infer: cx.get::<InferQuery>(owner)?,
            builtins: cx.get::<BuiltinsQuery>(&package)?,
            diagnostics: Vec::new(),
        };
        flow::Flow::new(&mut checker)?.run()?;
        rules::Rules::new(&mut checker)?.run()?;
        diff::run(&mut checker)?;
        Ok(checker.diagnostics.into())
    }
}

pub(crate) struct Checker<'a, 'b> {
    pub(crate) cx: &'a Cx<'b>,
    pub(crate) owner: BodyOwner,
    pub(crate) body: Arc<Body>,
    pub(crate) infer: Arc<InferenceResult>,
    pub(crate) builtins: Arc<Builtins>,
    pub(crate) diagnostics: Vec<InferDiagnostic>,
}

/// How far the patterns of a `switch` cover its input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Coverage {
    Complete,
    /// These values (as written) are not covered.
    Missing(Vec<String>),
    /// Only a discard or `var` pattern can cover this type.
    Open,
}

/// The values a pattern matches, as far as exhaustiveness goes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Cover {
    All,
    Some(BTreeSet<Value>),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Value {
    Bool(bool),
    Member(ItemId),
}

impl Cover {
    fn none() -> Cover {
        Cover::Some(BTreeSet::new())
    }

    fn union(self, other: Cover) -> Cover {
        match (self, other) {
            (Cover::All, _) | (_, Cover::All) => Cover::All,
            (Cover::Some(mut a), Cover::Some(b)) => {
                a.extend(b);
                Cover::Some(a)
            }
        }
    }
}

impl Checker<'_, '_> {
    pub(crate) fn report(&mut self, loc: InferLoc, code: &'static str, message: impl Into<String>) {
        self.diagnostics.push(InferDiagnostic {
            owner: self.owner,
            loc,
            code,
            message: message.into(),
        });
    }

    /// `(element, length)` of a `Vector<T, N>`.
    pub(crate) fn vector_of(&self, ty: &Ty) -> Option<(Ty, ConstTy)> {
        match ty {
            Ty::Adt { item, args } if self.builtins.kind(*item) == Some(BuiltinKind::Vector) => {
                match &args[..] {
                    [GenericArgTy::Type(t), GenericArgTy::Const(n)] => Some((t.clone(), n.clone())),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Whether `patterns` (those without a `when` guard) cover every value of `ty`.
    pub(crate) fn coverage(&self, ty: &Ty, patterns: &[PatId]) -> Result<Coverage, Cancelled> {
        let cover = patterns
            .iter()
            .fold(Cover::none(), |acc, &p| acc.union(self.cover(p)));
        let covered = match cover {
            Cover::All => return Ok(Coverage::Complete),
            Cover::Some(values) => values,
        };
        let Ty::Adt { item, .. } = ty else {
            return Ok(Coverage::Open);
        };
        if self.builtins.scalar(*item) == Some(Scalar::Bool) {
            let missing: Vec<String> = [true, false]
                .into_iter()
                .filter(|b| !covered.contains(&Value::Bool(*b)))
                .map(|b| b.to_string())
                .collect();
            return Ok(if missing.is_empty() {
                Coverage::Complete
            } else {
                Coverage::Missing(missing)
            });
        }
        let Some(members) = self.enum_members(*item)? else {
            return Ok(Coverage::Open);
        };
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        let enum_name = tree.items[item.local]
            .name
            .as_ref()
            .map_or(String::new(), |n| n.to_string());
        let missing: Vec<String> = members
            .into_iter()
            .filter(|m| !covered.contains(&Value::Member(*m)))
            .map(|m| {
                let name = tree.items[m.local]
                    .name
                    .as_ref()
                    .map_or(String::new(), |n| n.to_string());
                format!("{enum_name}.{name}")
            })
            .collect();
        Ok(if missing.is_empty() {
            Coverage::Complete
        } else {
            Coverage::Missing(missing)
        })
    }

    /// The members of an enum that is not `[Flags]`; `None` for other types.
    pub(crate) fn enum_members(&self, item: ItemId) -> Result<Option<Vec<ItemId>>, Cancelled> {
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        let data = &tree.items[item.local];
        if data.kind != ItemKind::Enum
            || self
                .cx
                .get::<ItemAttributesQuery>(&item)?
                .has(KnownAttr::Flags)
        {
            return Ok(None);
        }
        Ok(Some(
            data.members
                .iter()
                .filter(|&&m| tree.items[m].kind == ItemKind::EnumMember)
                .map(|&m| ItemId {
                    file: item.file,
                    local: m,
                })
                .collect(),
        ))
    }

    fn cover(&self, p: PatId) -> Cover {
        let store = &self.body.store;
        match &store.pats[p] {
            Pat::Discard | Pat::Var(_) => Cover::All,
            Pat::Constant(e) => match &store.exprs[*e] {
                Expr::Literal(Literal::Bool(b)) => Cover::Some([Value::Bool(*b)].into()),
                _ => match self.infer.paths.get(*e) {
                    Some(path) if path.steps.is_empty() => match path.root {
                        Root::Item(member) => Cover::Some([Value::Member(member)].into()),
                        _ => Cover::none(),
                    },
                    _ => Cover::none(),
                },
            },
            Pat::Or(a, b) => self.cover(*a).union(self.cover(*b)),
            Pat::And(a, b) => match (self.cover(*a), self.cover(*b)) {
                (Cover::All, x) | (x, Cover::All) => x,
                _ => Cover::none(),
            },
            // `{ }` and `( _, _ )` with nothing to test match everything.
            Pat::Property(items) if items.iter().all(|(_, s)| self.cover(*s) == Cover::All) => {
                Cover::All
            }
            Pat::Positional(items) if items.iter().all(|s| self.cover(*s) == Cover::All) => {
                Cover::All
            }
            _ => Cover::none(),
        }
    }
}
