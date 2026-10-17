//! Local scopes of a body, with C#'s rules:
//! - a local is visible from its declaration to the end of its block;
//! - local functions are visible in their whole block;
//! - variables declared inside an expression (`out var x`, `is var y`, deconstruction) are
//!   visible from there to the end of the enclosing statement and, for expression statements,
//!   declarations and `if` conditions, after it in the block; inside loop headers, `switch`
//!   sections and lambdas they stay inside;
//! - two locals of one name may not have declaration spaces where one contains the other (a
//!   block's locals cover the whole block, even before their declaration).
//!
//! Lookup is from the use outward, so a name used before its local's declaration is not that
//! local here; `hir_ty` reports such uses.

use std::sync::Arc;

use marlin_lang_arena::{Arena, ArenaMap, Idx};
use marlin_lang_query::{Cancelled, Cx, Query};

use crate::Name;
use crate::body::{Body, BodyOwner, BodyQuery};
use crate::store::*;

pub type ScopeId = Idx<ScopeData>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeData {
    pub parent: Option<ScopeId>,
    pub entries: Vec<(Name, BindingId)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExprScopes {
    pub scopes: Arena<ScopeData>,
    expr_scopes: ArenaMap<Expr, ScopeId>,
    stmt_scopes: ArenaMap<Stmt, ScopeId>,
    /// `(binding, earlier binding of the same name in an enclosing scope)`.
    pub redeclared: Vec<(BindingId, BindingId)>,
}

impl ExprScopes {
    pub fn new(body: &Body) -> Self {
        let mut scopes = Arena::new();
        let root = scopes.alloc(ScopeData {
            parent: None,
            entries: Vec::new(),
        });
        let mut builder = Builder {
            store: &body.store,
            this: ExprScopes {
                scopes,
                expr_scopes: ArenaMap::new(),
                stmt_scopes: ArenaMap::new(),
                redeclared: Vec::new(),
            },
            spaces: std::collections::HashSet::new(),
            declared: Vec::new(),
        };
        let mut scope = root;
        for &param in &body.params {
            builder.declare(&mut scope, param);
        }
        if let Some(init) = body.constructor_initializer {
            builder.expr(&mut scope, init);
        }
        match body.root {
            Some(LambdaBody::Expr(e)) => builder.expr(&mut scope, e),
            Some(LambdaBody::Block(s)) => builder.stmt(&mut scope, s),
            None => {}
        }
        builder.find_redeclarations();
        builder.this
    }

    /// The scope an expression is evaluated in.
    pub fn scope_of_expr(&self, expr: ExprId) -> Option<ScopeId> {
        self.expr_scopes.get(expr).copied()
    }

    pub fn scope_of_stmt(&self, stmt: StmtId) -> Option<ScopeId> {
        self.stmt_scopes.get(stmt).copied()
    }

    /// The innermost local named `name` visible in `scope`.
    pub fn resolve(&self, scope: ScopeId, name: &Name) -> Option<BindingId> {
        let mut at = Some(scope);
        while let Some(s) = at {
            let data = &self.scopes[s];
            if let Some(&(_, binding)) = data.entries.iter().rev().find(|(n, _)| n == name) {
                return Some(binding);
            }
            at = data.parent;
        }
        None
    }
}

struct Builder<'a> {
    store: &'a ExprStore,
    this: ExprScopes,
    /// Scopes that start a declaration space: blocks, loops, switch sections, lambdas, local
    /// functions, switch arms.
    spaces: std::collections::HashSet<ScopeId>,
    /// Every declaration and the scope it opened.
    declared: Vec<(BindingId, ScopeId)>,
}

impl Builder<'_> {
    /// A scope that starts a new declaration space.
    fn block(&mut self, parent: ScopeId) -> ScopeId {
        let scope = self.child(parent);
        self.spaces.insert(scope);
        scope
    }

    /// C# forbids two locals of one name when the declaration space of one contains the
    /// other's, whatever their order (a block's locals cover the whole block).
    fn find_redeclarations(&mut self) {
        let space_path = |this: &ExprScopes, mut scope: ScopeId| {
            let mut path = Vec::new();
            loop {
                if self.spaces.contains(&scope) {
                    path.push(scope);
                }
                match this.scopes[scope].parent {
                    Some(parent) => scope = parent,
                    None => break,
                }
            }
            path.reverse();
            path
        };
        let paths: Vec<(BindingId, Vec<ScopeId>)> = self
            .declared
            .iter()
            .map(|&(b, s)| (b, space_path(&self.this, s)))
            .collect();
        for (j, (later, later_path)) in paths.iter().enumerate() {
            let name = &self.store.bindings[*later].name;
            let earlier = paths[..j].iter().find(|(b, path)| {
                &self.store.bindings[*b].name == name
                    && (path.starts_with(later_path) || later_path.starts_with(path))
            });
            if let Some((previous, _)) = earlier {
                self.this.redeclared.push((*later, *previous));
            }
        }
    }

    fn child(&mut self, parent: ScopeId) -> ScopeId {
        self.this.scopes.alloc(ScopeData {
            parent: Some(parent),
            entries: Vec::new(),
        })
    }

    /// Declares `binding` in a new scope after `scope`, and moves `scope` there.
    fn declare(&mut self, scope: &mut ScopeId, binding: BindingId) {
        let name = &self.store.bindings[binding].name;
        if name.as_str() == "_" {
            return;
        }
        let next = self.child(*scope);
        self.this.scopes[next].entries.push((name.clone(), binding));
        self.declared.push((binding, next));
        *scope = next;
    }

    fn stmt(&mut self, scope: &mut ScopeId, stmt: StmtId) {
        self.this.stmt_scopes.insert(stmt, *scope);
        match &self.store.stmts[stmt] {
            Stmt::Missing | Stmt::Break | Stmt::Continue | Stmt::Empty => {}
            Stmt::Expr(e) => self.expr(scope, *e),
            Stmt::Local {
                attributes,
                declarators,
                ..
            } => {
                for attribute in attributes {
                    for arg in &attribute.args {
                        self.expr(&mut scope.clone(), arg.expr);
                    }
                }
                for &(binding, init) in declarators {
                    if let Some(init) = init {
                        self.expr(scope, init);
                    }
                    self.declare(scope, binding);
                }
            }
            Stmt::Block(stmts) => {
                let mut inner = self.block(*scope);
                // Local functions are visible in the whole block.
                for &s in stmts {
                    if let Stmt::LocalFunction(f) = &self.store.stmts[s] {
                        self.declare(&mut inner, f.binding);
                    }
                }
                for &s in stmts {
                    self.stmt(&mut inner, s);
                }
            }
            Stmt::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                // Variables declared in the condition stay visible after the `if`.
                self.expr(scope, *cond);
                self.stmt(&mut scope.clone(), *then);
                if let Some(otherwise) = otherwise {
                    self.stmt(&mut scope.clone(), *otherwise);
                }
            }
            Stmt::For {
                init,
                cond,
                update,
                body,
            } => {
                let mut inner = self.block(*scope);
                for &s in init {
                    self.stmt(&mut inner, s);
                }
                if let Some(cond) = cond {
                    self.expr(&mut inner, *cond);
                }
                self.stmt(&mut inner.clone(), *body);
                for &u in update {
                    self.expr(&mut inner.clone(), u);
                }
            }
            Stmt::Foreach {
                variable,
                iterable,
                body,
            } => {
                let mut inner = *scope;
                self.expr(&mut inner.clone(), *iterable);
                self.expr(&mut inner, *variable);
                self.stmt(&mut inner, *body);
            }
            Stmt::While { cond, body } => {
                let mut inner = *scope;
                self.expr(&mut inner, *cond);
                self.stmt(&mut inner, *body);
            }
            Stmt::Do { body, cond } => {
                self.stmt(&mut scope.clone(), *body);
                self.expr(&mut scope.clone(), *cond);
            }
            Stmt::Switch {
                scrutinee,
                sections,
            } => {
                self.expr(&mut scope.clone(), *scrutinee);
                for section in sections {
                    let mut inner = self.block(*scope);
                    for &(pat, guard) in section.labels.iter().flatten() {
                        self.pat(&mut inner, pat);
                        if let Some(guard) = guard {
                            self.expr(&mut inner, guard);
                        }
                    }
                    for &s in &section.body {
                        self.stmt(&mut inner, s);
                    }
                }
            }
            Stmt::Return(e) => {
                if let Some(e) = e {
                    self.expr(scope, *e);
                }
            }
            Stmt::LocalFunction(f) => {
                let mut inner = self.block(*scope);
                for param in &f.params {
                    self.declare(&mut inner, param.binding);
                }
                match f.body {
                    Some(LambdaBody::Expr(e)) => self.expr(&mut inner, e),
                    Some(LambdaBody::Block(s)) => self.stmt(&mut inner, s),
                    None => {}
                }
            }
        }
    }

    /// Walks an expression in `scope`; declarations inside move `scope` forward.
    fn expr(&mut self, scope: &mut ScopeId, expr: ExprId) {
        self.this.expr_scopes.insert(expr, *scope);
        let store = self.store;
        match &store.exprs[expr] {
            Expr::Missing
            | Expr::Literal(_)
            | Expr::Path(_)
            | Expr::Type(_)
            | Expr::This
            | Expr::Base
            | Expr::Default(_)
            | Expr::Typeof(_)
            | Expr::Sizeof(_) => {}
            Expr::Interpolated(parts) => {
                for part in parts {
                    if let InterpolationPart::Hole {
                        expr, alignment, ..
                    } = part
                    {
                        self.expr(scope, *expr);
                        if let Some(a) = alignment {
                            self.expr(scope, *a);
                        }
                    }
                }
            }
            Expr::Member { receiver, .. } => self.expr(scope, *receiver),
            Expr::Call { callee, args } => {
                self.expr(scope, *callee);
                for arg in args {
                    self.expr(scope, arg.expr);
                }
            }
            Expr::Index { base, args } => {
                self.expr(scope, *base);
                for arg in args {
                    self.expr(scope, arg.expr);
                }
            }
            Expr::Unary { operand, .. } => self.expr(scope, *operand),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(scope, *lhs);
                self.expr(scope, *rhs);
            }
            Expr::Assign { target, value, .. } => {
                // `var (a, b) = t;` declares after evaluating the value.
                self.expr(scope, *value);
                self.expr(scope, *target);
            }
            Expr::Conditional {
                cond,
                then,
                otherwise,
            } => {
                self.expr(scope, *cond);
                self.expr(&mut scope.clone(), *then);
                self.expr(&mut scope.clone(), *otherwise);
            }
            Expr::Cast { expr, .. } | Expr::As { expr, .. } => self.expr(scope, *expr),
            Expr::Range { start, end } => {
                for e in [start, end].into_iter().flatten() {
                    self.expr(scope, *e);
                }
            }
            Expr::Is { expr, pat } => {
                self.expr(scope, *expr);
                self.pat(scope, *pat);
            }
            Expr::Switch { scrutinee, arms } => {
                self.expr(scope, *scrutinee);
                for arm in arms {
                    let mut inner = self.block(*scope);
                    self.pat(&mut inner, arm.pat);
                    if let Some(guard) = arm.guard {
                        self.expr(&mut inner, guard);
                    }
                    self.expr(&mut inner, arm.expr);
                }
            }
            Expr::With { expr, inits } => {
                self.expr(scope, *expr);
                for init in inits {
                    self.expr(scope, init.value);
                }
            }
            Expr::ObjectInit { inits, .. } => {
                for init in inits {
                    self.expr(scope, init.value);
                }
            }
            Expr::New { args, inits, .. } => {
                for arg in args.iter().flatten() {
                    self.expr(scope, arg.expr);
                }
                for init in inits.iter().flatten() {
                    self.expr(scope, init.value);
                }
            }
            Expr::Throw(e) => {
                if let Some(e) = e {
                    self.expr(scope, *e);
                }
            }
            Expr::Collection(items) => {
                for &e in items {
                    self.expr(scope, e);
                }
            }
            Expr::Tuple(items) => {
                for &(_, e) in items {
                    self.expr(scope, e);
                }
            }
            Expr::Lambda { params, body } => {
                let mut inner = self.block(*scope);
                for param in params {
                    self.declare(&mut inner, param.binding);
                }
                match body {
                    LambdaBody::Expr(e) => self.expr(&mut inner, *e),
                    LambdaBody::Block(s) => self.stmt(&mut inner, *s),
                }
            }
            Expr::DeclVar { binding, .. } => self.declare(scope, *binding),
            Expr::DeclTuple(bindings) => {
                for &binding in bindings {
                    self.declare(scope, binding);
                }
            }
        }
    }

    fn pat(&mut self, scope: &mut ScopeId, pat: PatId) {
        match &self.store.pats[pat] {
            Pat::Missing | Pat::Discard => {}
            Pat::Constant(e) | Pat::Relational { value: e, .. } => self.expr(scope, *e),
            Pat::Var(binding) => self.declare(scope, *binding),
            Pat::And(a, b) | Pat::Or(a, b) => {
                self.pat(scope, *a);
                self.pat(scope, *b);
            }
            Pat::Not(p) => self.pat(scope, *p),
            Pat::Property(items) => {
                for &(_, p) in items {
                    self.pat(scope, p);
                }
            }
            Pat::Positional(items) => {
                for &p in items {
                    self.pat(scope, p);
                }
            }
        }
    }
}

/// The local scopes of a body.
pub struct ExprScopesQuery;
impl Query for ExprScopesQuery {
    type Key = BodyOwner;
    type Value = Arc<ExprScopes>;
    fn execute(cx: &Cx<'_>, owner: &BodyOwner) -> Result<Arc<ExprScopes>, Cancelled> {
        let body = cx.get::<BodyQuery>(owner)?;
        Ok(Arc::new(ExprScopes::new(&body)))
    }
}
