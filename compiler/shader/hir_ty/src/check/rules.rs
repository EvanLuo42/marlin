//! Read-only values (0004 §5) and exhaustive `switch` (§9.1).
//!
//! A value is read-only when it is an `in` parameter, a constant, a `foreach` variable, a
//! `readonly` field or anything inside one, or `this` in a `readonly` member or a member of a
//! `readonly struct` (constructors may still write their own fields). Writing it is an error
//! (assignment, `++`/`--`, passing it as `ref` or `out`), and so is calling a method on it that
//! is not `readonly`: C# would quietly call the method on a copy, which we do not do.

use std::collections::HashSet;

use marlin_front_hir_def::store::{BindingId, BindingKind, Expr, ExprId, RefKind, Stmt, UnaryOp};
use marlin_front_hir_def::{ItemId, ItemKind, ItemTreeQuery};
use marlin_lang_query::Cancelled;

use super::types::type_problems;
use super::{Checker, Coverage};
use crate::infer::{Callee, InferLoc, Root, Step, ValuePath};
use crate::item::ItemTypesQuery;
use crate::ty::Ty;

pub(super) struct Rules<'c, 'a, 'b> {
    checker: &'c mut Checker<'a, 'b>,
    foreach_variables: HashSet<BindingId>,
    /// Why `this` is read-only in the body, if it is.
    this_readonly: Option<&'static str>,
    /// The struct whose constructor this body is: its `readonly` fields can be written here.
    constructing: Option<ItemId>,
}

impl<'c, 'a, 'b> Rules<'c, 'a, 'b> {
    pub(super) fn new(checker: &'c mut Checker<'a, 'b>) -> Result<Self, Cancelled> {
        let item = checker.owner.item;
        let tree = checker.cx.get::<ItemTreeQuery>(&item.file)?;
        let data = &tree.items[item.local];
        let parent = data.parent.map(|p| &tree.items[p]);
        let constructing = (data.kind == ItemKind::Constructor)
            .then_some(data.parent)
            .flatten()
            .map(|local| ItemId {
                file: item.file,
                local,
            });
        let this_readonly = if constructing.is_some() {
            None
        } else if parent.is_some_and(|p| p.is_readonly) {
            Some("`this`, which is read-only in a `readonly struct`")
        } else if data.is_readonly {
            Some("`this`, which is read-only in a `readonly` member")
        } else {
            None
        };
        let mut foreach_variables = HashSet::new();
        for (_, stmt) in checker.body.store.stmts.iter() {
            if let Stmt::Foreach { variable, .. } = stmt {
                match &checker.body.store.exprs[*variable] {
                    Expr::DeclVar { binding, .. } => {
                        foreach_variables.insert(*binding);
                    }
                    Expr::DeclTuple(bindings) => foreach_variables.extend(bindings.iter().copied()),
                    _ => {}
                }
            }
        }
        Ok(Rules {
            checker,
            foreach_variables,
            this_readonly,
            constructing,
        })
    }

    pub(super) fn run(mut self) -> Result<(), Cancelled> {
        let body = self.checker.body.clone();
        // The types of locals follow the rules of types in signatures; parameters of the
        // owner are checked with its signature.
        for (b, binding) in body.store.bindings.iter() {
            let is_param = match binding.kind {
                BindingKind::LambdaParam | BindingKind::LocalFunctionParam => true,
                BindingKind::Local { .. } | BindingKind::Declared => false,
                _ => continue,
            };
            let Some(ty) = self.checker.infer.binding_types.get(b).cloned() else {
                continue;
            };
            let problems = type_problems(self.checker.cx, &self.checker.builtins, &ty, is_param)?;
            for p in problems {
                self.checker.report(InferLoc::Binding(b), p.code, p.message);
            }
        }
        for (e, expr) in body.store.exprs.iter() {
            match expr {
                Expr::Assign { target, .. } => self.write(*target, "assign to")?,
                Expr::Unary {
                    op: UnaryOp::PreInc | UnaryOp::PreDec | UnaryOp::PostInc | UnaryOp::PostDec,
                    operand,
                } => self.write(*operand, "modify")?,
                Expr::Call { callee, args } => {
                    for arg in args {
                        match arg.modifier {
                            Some(RefKind::Ref) => self.write(arg.expr, "pass as `ref`")?,
                            Some(RefKind::Out) => self.write(arg.expr, "pass as `out`")?,
                            _ => {}
                        }
                    }
                    self.call(e, *callee)?;
                }
                Expr::Switch { scrutinee, arms } => {
                    let unguarded: Vec<_> = arms
                        .iter()
                        .filter(|a| a.guard.is_none())
                        .map(|a| a.pat)
                        .collect();
                    self.exhaustive(*scrutinee, &unguarded, true)?;
                }
                _ => {}
            }
        }
        for (_, stmt) in body.store.stmts.iter() {
            if let Stmt::Switch {
                scrutinee,
                sections,
            } = stmt
            {
                let labels = || sections.iter().flat_map(|s| s.labels.iter());
                if labels().any(Option::is_none) {
                    continue;
                }
                let unguarded: Vec<_> = labels()
                    .flatten()
                    .filter(|(_, guard)| guard.is_none())
                    .map(|(p, _)| *p)
                    .collect();
                self.exhaustive(*scrutinee, &unguarded, false)?;
            }
        }
        Ok(())
    }

    // ---- exhaustiveness ----

    /// Switch expressions must cover every value; switch statements every member of an enum
    /// that is not `[Flags]`.
    fn exhaustive(
        &mut self,
        scrutinee: ExprId,
        patterns: &[marlin_front_hir_def::store::PatId],
        is_expr: bool,
    ) -> Result<(), Cancelled> {
        let ty = self.checker.infer.type_of(scrutinee);
        if ty.is_error() {
            return Ok(());
        }
        if !is_expr {
            let Ty::Adt { item, .. } = &ty else {
                return Ok(());
            };
            if self.checker.enum_members(*item)?.is_none() {
                return Ok(());
            }
        }
        let message = match self.checker.coverage(&ty, patterns)? {
            Coverage::Complete => return Ok(()),
            Coverage::Missing(missing) => {
                let list: Vec<String> = missing.iter().map(|m| format!("`{m}`")).collect();
                let fix = if is_expr { "`_ =>`" } else { "`default:`" };
                format!(
                    "this `switch` does not cover {}; add them or {fix}",
                    list.join(", ")
                )
            }
            Coverage::Open => {
                "this `switch` expression does not cover every value; add `_ =>`".to_string()
            }
        };
        self.checker
            .report(InferLoc::Expr(scrutinee), "E1131", message);
        Ok(())
    }

    // ---- read-only values ----

    /// A write to the place `e`.
    fn write(&mut self, e: ExprId, verb: &str) -> Result<(), Cancelled> {
        let store = &self.checker.body.store;
        match &store.exprs[e] {
            Expr::Tuple(items) => {
                for (_, x) in items.clone() {
                    self.write(x, verb)?;
                }
                return Ok(());
            }
            // An element of a read-only array.
            Expr::Index { base, .. } => {
                let base = *base;
                if matches!(self.checker.infer.type_of(base), Ty::Array { .. })
                    && let Some(path) = self.checker.infer.paths.get(base).cloned()
                    && let Some(reason) = self.readonly(&path)?
                {
                    self.checker.report(
                        InferLoc::Expr(e),
                        "E1129",
                        format!("cannot {verb} an element of {reason}"),
                    );
                }
                return Ok(());
            }
            _ => {}
        }
        let Some(path) = self.checker.infer.paths.get(e).cloned() else {
            return Ok(());
        };
        if let Some(reason) = self.readonly(&path)? {
            self.checker.report(
                InferLoc::Expr(e),
                "E1129",
                format!("cannot {verb} {reason}"),
            );
        }
        Ok(())
    }

    /// A method that is not `readonly` called on a read-only value.
    fn call(&mut self, e: ExprId, callee: ExprId) -> Result<(), Cancelled> {
        let Some(Callee::Item { item, .. }) = self.checker.infer.callees.get(e).cloned() else {
            return Ok(());
        };
        let tree = self.checker.cx.get::<ItemTreeQuery>(&item.file)?;
        let data = &tree.items[item.local];
        let Some(parent) = data.parent else {
            return Ok(());
        };
        let mutating = data.kind == ItemKind::Function
            && !data.is_static
            && !data.is_readonly
            && tree.items[parent].kind == ItemKind::Struct
            && !tree.items[parent].is_readonly;
        if !mutating {
            return Ok(());
        }
        let this = ValuePath {
            root: Root::This,
            steps: Vec::new(),
        };
        let store = &self.checker.body.store;
        let receiver = match &store.exprs[callee] {
            Expr::Path(path) if path.segments.len() > 1 => {
                self.checker.infer.paths.get(callee).cloned()
            }
            // `Method()` on the implicit `this`.
            Expr::Path(_) => Some(this),
            Expr::Member { receiver, .. } => match store.exprs[*receiver] {
                Expr::This => Some(this),
                _ => self.checker.infer.paths.get(*receiver).cloned(),
            },
            _ => None,
        };
        let Some(receiver) = receiver else {
            return Ok(());
        };
        if let Some(reason) = self.readonly(&receiver)? {
            let name = data.name.as_ref().map_or(String::new(), |n| n.to_string());
            self.checker.report(
                InferLoc::Expr(callee),
                "E1130",
                format!("`{name}` is not `readonly`, so it cannot be called on {reason}"),
            );
        }
        Ok(())
    }

    /// Why the place `path` is read-only, as "`x`, an `in` parameter"; `None` if it is not.
    fn readonly(&self, path: &ValuePath) -> Result<Option<String>, Cancelled> {
        let cx = self.checker.cx;
        let store = &self.checker.body.store;
        let root = match path.root {
            Root::Local(b) => {
                let binding = &store.bindings[b];
                let name = &binding.name;
                match binding.kind {
                    BindingKind::Param(i) => {
                        let types = cx.get::<ItemTypesQuery>(&self.checker.owner.item)?;
                        (types.params.get(i as usize).and_then(|p| p.ref_kind) == Some(RefKind::In))
                            .then(|| format!("`{name}`, an `in` parameter"))
                    }
                    BindingKind::Local { is_const: true } => Some(format!("`{name}`, a constant")),
                    _ if self.foreach_variables.contains(&b) => {
                        Some(format!("`{name}`, a `foreach` variable"))
                    }
                    _ => None,
                }
            }
            Root::This => self.this_readonly.map(str::to_string),
            Root::Item(item) => {
                let tree = cx.get::<ItemTreeQuery>(&item.file)?;
                let data = &tree.items[item.local];
                let name = data.name.as_ref().map_or(String::new(), |n| n.to_string());
                if data.is_const || data.kind == ItemKind::EnumMember {
                    Some(format!("`{name}`, a constant"))
                } else if data.is_readonly {
                    Some(format!("`{name}`, a `readonly` field"))
                } else {
                    None
                }
            }
            Root::ConstParam { owner, index } => {
                let types = cx.get::<ItemTypesQuery>(&owner)?;
                let name = types
                    .generics
                    .get(index as usize)
                    .map_or(String::new(), |g| g.name.to_string());
                Some(format!("`{name}`, a constant generic parameter"))
            }
        };
        if root.is_some() {
            return Ok(root);
        }
        for (i, step) in path.steps.iter().enumerate() {
            match step {
                Step::Field(field) => {
                    let tree = cx.get::<ItemTreeQuery>(&field.file)?;
                    let data = &tree.items[field.local];
                    let owner = data.parent.map(|local| ItemId {
                        file: field.file,
                        local,
                    });
                    let in_own_constructor =
                        i == 0 && path.root == Root::This && owner == self.constructing;
                    let readonly_struct = data.parent.is_some_and(|p| tree.items[p].is_readonly);
                    if (data.is_readonly || readonly_struct) && !in_own_constructor {
                        let name = data.name.as_ref().map_or(String::new(), |n| n.to_string());
                        return Ok(Some(format!("`{name}`, a `readonly` field")));
                    }
                }
                Step::Length => return Ok(Some("an array's length".to_string())),
                Step::Property(_) | Step::Swizzle(_) => {}
            }
        }
        Ok(None)
    }
}
