//! Type inference of bodies (step 5b).
//!
//! C#-style and bidirectional: an expression is checked against an expected type when there is
//! one (a declared local, a parameter, a return type), which is how literals, target-typed
//! `{ ... }`, `default` and lambdas get their types. Everything is recorded per HIR id in
//! [`InferenceResult`] for MIR lowering: types, what paths and members name, the chosen callee
//! of calls and operators, and implicit conversions.

mod bounds;
mod call;
mod conv;
mod diff;
mod expr;
mod group;
mod member;
mod ops;
mod stmt;

use std::sync::Arc;

use marlin_front_hir_def::store::{
    BindingId, Expr, ExprId, Literal, PatId, StmtId, TypeRefId, UnaryOp,
};
use marlin_front_hir_def::{
    Body, BodyOwner, BodyQuery, ExprScopes, ExprScopesQuery, FilePackage, ItemId, ItemKind,
    ItemTreeQuery, Resolver,
};
use marlin_lang_arena::ArenaMap;
use marlin_lang_query::{Cancelled, Cx, Query};

use crate::builtins::{BuiltinKind, Builtins, BuiltinsQuery, Scalar};
use crate::diagnostics::TyDiagnostic;
use crate::item::{ItemTypesQuery, self_ty};
use crate::lower::TyLowerer;
use crate::subst::Subst;
use crate::ty::{ConstTy, GenericArgTy, Ty};

pub use member::Step;

/// Where a value comes from, for a path or member expression.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Root {
    Local(BindingId),
    /// The receiver of the method being checked (`State` inside `Rng.Next`).
    This,
    /// A namespace-level or static field, a constant, an enum member.
    Item(ItemId),
    /// A constant generic parameter (`N` of `Sum<T, const int N>`).
    ConstParam {
        owner: ItemId,
        index: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ValuePath {
    pub root: Root,
    pub steps: Vec<Step>,
}

/// What a call, operator, indexer or constructor expression invokes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Callee {
    /// A declared function, method, operator, constructor or indexer accessor, with the
    /// arguments of its own and its type's generic parameters.
    Item { item: ItemId, subst: Subst },
    /// A built-in operator on scalars, vectors or matrices.
    BuiltinOp,
    /// `float3(a, b)`, `float(x)`: building or converting a built-in value.
    BuiltinConstruct(Ty),
    /// `T()` or `T { ... }` of a struct without a declared constructor: its field defaults.
    DefaultConstruct(Ty),
    /// Calling a `Func`/`Action` parameter.
    Invoke(BindingId),
    /// `ForwardDiff(f)(args)` (0004 §15.2): the derivative of `f`, a function with its type
    /// arguments; `call_args` maps its parameters to the arguments (a `DiffPair` for each
    /// parameter that takes part in differentiation).
    ForwardDiff { item: ItemId, subst: Subst },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum InferLoc {
    Expr(ExprId),
    Type(TypeRefId),
    Pat(PatId),
    Stmt(StmtId),
    Binding(BindingId),
    /// The body as a whole (its owner's name).
    Owner,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InferDiagnostic {
    pub owner: BodyOwner,
    pub loc: InferLoc,
    pub code: &'static str,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InferenceResult {
    pub expr_types: ArenaMap<Expr, Ty>,
    pub binding_types: ArenaMap<marlin_front_hir_def::store::Binding, Ty>,
    pub paths: ArenaMap<Expr, ValuePath>,
    pub callees: ArenaMap<Expr, Callee>,
    /// Expressions converted implicitly, to the type they were converted to.
    pub coercions: ArenaMap<Expr, Ty>,
    /// For calls, constructors and indexers resolved to a declaration: per parameter of the
    /// callee, the index of the argument passed to it, or `None` where its default is used.
    /// (Operators take their operands in order.)
    pub call_args: ArenaMap<Expr, Vec<Option<u32>>>,
    /// What each `.name` member expression reads (also where its receiver has no path).
    pub member_steps: ArenaMap<Expr, Step>,
    /// For method calls (`r.M()`, `T.M()`): the type the method was looked up on.
    pub receivers: ArenaMap<Expr, Ty>,
    /// Functions passed as a `Func` or `Action` (`Integrate(2, Square)`), with their type
    /// arguments.
    pub method_groups: ArenaMap<Expr, (ItemId, Subst)>,
    /// What the body produces: a function's result, `void` for constructors and setters,
    /// `None` for initializers.
    pub body_ty: Option<Ty>,
    pub diagnostics: Vec<InferDiagnostic>,
}

impl InferenceResult {
    pub fn type_of(&self, e: ExprId) -> Ty {
        self.expr_types.get(e).cloned().unwrap_or(Ty::Error)
    }
}

/// Infers the types of one body.
pub struct InferQuery;
impl Query for InferQuery {
    type Key = BodyOwner;
    type Value = Arc<InferenceResult>;
    fn execute(cx: &Cx<'_>, owner: &BodyOwner) -> Result<Arc<InferenceResult>, Cancelled> {
        let body = cx.get::<BodyQuery>(owner)?;
        let scopes = cx.get::<ExprScopesQuery>(owner)?;
        let package = cx.input::<FilePackage>(&owner.item.file)?;
        let builtins = cx.get::<BuiltinsQuery>(&package)?;
        let resolver = Resolver::for_item(cx, owner.item)?;
        let mut ctx = InferCtx {
            cx,
            owner: *owner,
            body: body.clone(),
            scopes,
            resolver,
            builtins,
            result: InferenceResult::default(),
            return_ty: None,
            this_ty: None,
            local_functions: std::collections::HashMap::new(),
        };
        ctx.run()?;
        Ok(Arc::new(ctx.result))
    }
}

/// A literal's flexibility: unsuffixed literals adapt to the expected numeric type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lit {
    None,
    /// An unsuffixed integer literal with this value.
    Int(i128),
    /// An unsuffixed real literal.
    Float,
}

pub(crate) struct InferCtx<'a, 'b> {
    pub(crate) cx: &'a Cx<'b>,
    pub(crate) owner: BodyOwner,
    pub(crate) body: Arc<Body>,
    pub(crate) scopes: Arc<ExprScopes>,
    pub(crate) resolver: Resolver,
    pub(crate) builtins: Arc<Builtins>,
    pub(crate) result: InferenceResult,
    /// What `return` must produce; `None` outside functions (initializers).
    pub(crate) return_ty: Option<Ty>,
    /// The type of `this` inside an instance member.
    pub(crate) this_ty: Option<Ty>,
    /// Parameter and result types of the local functions declared so far.
    pub(crate) local_functions: std::collections::HashMap<BindingId, (Vec<Ty>, Ty)>,
}

impl InferCtx<'_, '_> {
    fn run(&mut self) -> Result<(), Cancelled> {
        let item = self.owner.item;
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        let data = tree.items[item.local].clone();
        let types = self.cx.get::<ItemTypesQuery>(&item)?;

        // `this` for members of types (static members have none).
        if let Some(parent) = data.parent {
            let parent_id = ItemId {
                file: item.file,
                local: parent,
            };
            let parent_kind = tree.items[parent].kind;
            if parent_kind.is_type() && !data.is_static && !data.is_const {
                self.this_ty = Some(self_ty(self.cx, parent_id)?);
            }
            if parent_kind == ItemKind::Extension {
                let ext = self.cx.get::<ItemTypesQuery>(&parent_id)?;
                self.this_ty = ext.receiver.clone();
            }
        }

        // Parameter bindings.
        let params = self.body.params.clone();
        for binding in params {
            let kind = self.body.store.bindings[binding].kind;
            let ty = match kind {
                marlin_front_hir_def::store::BindingKind::Param(i) => types
                    .params
                    .get(i as usize)
                    .map_or(Ty::Error, |p| p.ty.clone()),
                marlin_front_hir_def::store::BindingKind::Receiver => {
                    self.this_ty.clone().unwrap_or(Ty::Error)
                }
                marlin_front_hir_def::store::BindingKind::SetterValue => {
                    types.ty.clone().unwrap_or(Ty::Error)
                }
                _ => Ty::Error,
            };
            self.result.binding_types.insert(binding, ty);
        }

        let value_ty = match (data.kind, self.owner.part) {
            (ItemKind::Constructor, _) => Some(Ty::Void),
            // C#: an enum member's value is a constant of the underlying type.
            (ItemKind::EnumMember, _) => match data.parent {
                Some(parent) => Some(self.underlying_ty(ItemId {
                    file: item.file,
                    local: parent,
                })?),
                None => Some(Ty::Error),
            },
            (
                ItemKind::Property | ItemKind::Indexer,
                marlin_front_hir_def::BodyPart::Accessor(_),
            ) => {
                // A getter returns the property's type; a setter returns nothing. The setter
                // is the accessor that has `value` in scope.
                let is_setter = self.body.params.iter().any(|&b| {
                    self.body.store.bindings[b].kind
                        == marlin_front_hir_def::store::BindingKind::SetterValue
                });
                if is_setter {
                    Some(Ty::Void)
                } else {
                    types.ty.clone()
                }
            }
            _ => types.ty.clone(),
        };
        self.return_ty = value_ty.clone();
        self.result.body_ty = value_ty.clone();

        if let Some(init) = self.body.constructor_initializer {
            self.infer_expr(init, None)?;
        }
        match self.body.root {
            Some(marlin_front_hir_def::store::LambdaBody::Expr(e)) => match &value_ty {
                Some(Ty::Void) => {
                    self.infer_expr(e, None)?;
                }
                Some(t) => {
                    let t = t.clone();
                    self.check_expr(e, &t)?;
                }
                None => {
                    self.infer_expr(e, None)?;
                }
            },
            Some(marlin_front_hir_def::store::LambdaBody::Block(s)) => self.check_stmt(s)?,
            None => {}
        }
        Ok(())
    }

    pub(crate) fn report(&mut self, loc: InferLoc, code: &'static str, message: impl Into<String>) {
        self.result.diagnostics.push(InferDiagnostic {
            owner: self.owner,
            loc,
            code,
            message: message.into(),
        });
    }

    /// Lowers a type written in the body.
    pub(crate) fn lower_type(&mut self, t: TypeRefId) -> Result<Ty, Cancelled> {
        let mut lower = TyLowerer {
            cx: self.cx,
            owner: self.owner.item,
            store: &self.body.store,
            resolver: self.resolver.clone(),
            builtins: self.builtins.clone(),
            diagnostics: Vec::new(),
            in_attribute: false,
        };
        let ty = lower.ty(t)?;
        let diagnostics = std::mem::take(&mut lower.diagnostics);
        for d in diagnostics {
            self.forward(d);
        }
        Ok(ty)
    }

    fn forward(&mut self, d: TyDiagnostic) {
        let loc = match d.loc {
            crate::TyLoc::Type(t) => InferLoc::Type(t),
            crate::TyLoc::Expr(e) => InferLoc::Expr(e),
            crate::TyLoc::Item => InferLoc::Owner,
        };
        let code = d.code().0;
        let message = d.message();
        self.report(loc, code, message);
    }

    /// `ty` as the author would write it.
    pub(crate) fn show(&self, ty: &Ty) -> String {
        crate::display_ty(self.cx, &self.builtins, ty).unwrap_or_else(|_| "?".into())
    }

    // ---- shapes of types ----

    pub(crate) fn scalar_of(&self, ty: &Ty) -> Option<Scalar> {
        match ty {
            Ty::Adt { item, .. } => self.builtins.scalar(*item),
            _ => None,
        }
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

    /// `(element, rows, columns)` of a `Matrix<T, R, C>`.
    pub(crate) fn matrix_of(&self, ty: &Ty) -> Option<(Ty, ConstTy, ConstTy)> {
        match ty {
            Ty::Adt { item, args } if self.builtins.kind(*item) == Some(BuiltinKind::Matrix) => {
                match &args[..] {
                    [
                        GenericArgTy::Type(t),
                        GenericArgTy::Const(r),
                        GenericArgTy::Const(c),
                    ] => Some((t.clone(), r.clone(), c.clone())),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub(crate) fn scalar_ty(&self, s: Scalar) -> Ty {
        self.builtins
            .item(BuiltinKind::Scalar(s))
            .map_or(Ty::Error, Ty::adt)
    }

    pub(crate) fn bool_ty(&self) -> Ty {
        self.scalar_ty(Scalar::Bool)
    }

    pub(crate) fn vector_ty(&self, element: Ty, n: ConstTy) -> Ty {
        match self.builtins.item(BuiltinKind::Vector) {
            Some(item) => Ty::Adt {
                item,
                args: vec![GenericArgTy::Type(element), GenericArgTy::Const(n)].into(),
            },
            None => Ty::Error,
        }
    }

    pub(crate) fn matrix_ty(&self, element: Ty, rows: ConstTy, cols: ConstTy) -> Ty {
        match self.builtins.item(BuiltinKind::Matrix) {
            Some(item) => Ty::Adt {
                item,
                args: vec![
                    GenericArgTy::Type(element),
                    GenericArgTy::Const(rows),
                    GenericArgTy::Const(cols),
                ]
                .into(),
            },
            None => Ty::Error,
        }
    }

    /// An enum's underlying type: the one written after `:`, else `int`.
    pub(crate) fn underlying_ty(&self, enum_item: ItemId) -> Result<Ty, Cancelled> {
        let types = self.cx.get::<ItemTypesQuery>(&enum_item)?;
        Ok(match types.bases.first() {
            Some(t) => t.clone(),
            None => self.scalar_ty(Scalar::I32),
        })
    }

    /// The type of the field or enum member `item` read here: inside an enum member's
    /// initializer, the members of the same enum have the underlying type (as in C#).
    pub(crate) fn in_enum_initializer(&self, item: ItemId, ty: Ty) -> Result<Ty, Cancelled> {
        if item.file != self.owner.item.file {
            return Ok(ty);
        }
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        let data = &tree.items[item.local];
        let owner = &tree.items[self.owner.item.local];
        let same_enum = data.kind == ItemKind::EnumMember
            && owner.kind == ItemKind::EnumMember
            && data.parent == owner.parent;
        match (same_enum, data.parent) {
            (true, Some(parent)) => self.underlying_ty(ItemId {
                file: item.file,
                local: parent,
            }),
            _ => Ok(ty),
        }
    }

    pub(crate) fn item_kind(&self, item: ItemId) -> Result<ItemKind, Cancelled> {
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        Ok(tree.items[item.local].kind)
    }

    /// The flexibility of an expression as a literal (looking through unary minus).
    pub(crate) fn literal_of(&self, e: ExprId) -> Lit {
        match &self.body.store.exprs[e] {
            Expr::Literal(Literal::Number(text)) => {
                let lower = text.to_ascii_lowercase();
                let is_hex = lower.starts_with("0x") || lower.starts_with("0b");
                if !is_hex && lower.contains(['.', 'e']) && !lower.ends_with(['f', 'd', 'h', 'm']) {
                    return Lit::Float;
                }
                if lower.ends_with(['u', 'l']) {
                    return Lit::None;
                }
                if !is_hex && lower.ends_with(['f', 'd', 'h', 'm']) {
                    return Lit::None;
                }
                crate::lower::parse_int(text).map_or(Lit::None, Lit::Int)
            }
            Expr::Unary {
                op: UnaryOp::Neg,
                operand,
            } => match self.literal_of(*operand) {
                Lit::Int(v) => Lit::Int(-v),
                other => other,
            },
            _ => Lit::None,
        }
    }

    pub(crate) fn record(&mut self, e: ExprId, ty: Ty) -> Ty {
        self.result.expr_types.insert(e, ty.clone());
        ty
    }
}

impl InferDiagnostic {
    /// The diagnostic with ranges in the file's current text.
    pub fn to_diagnostic(&self, cx: &Cx<'_>) -> Result<marlin_lang_diag::Diagnostic, Cancelled> {
        let (_, map) = cx.get::<marlin_front_hir_def::BodyWithSourceMap>(&self.owner)?;
        let ptr = match &self.loc {
            InferLoc::Expr(e) => map.exprs.get(*e).copied(),
            InferLoc::Type(t) => map.types.get(*t).copied(),
            InferLoc::Pat(p) => map.pats.get(*p).copied(),
            InferLoc::Stmt(s) => map.stmts.get(*s).copied(),
            InferLoc::Binding(b) => map.bindings.get(*b).copied(),
            InferLoc::Owner => None,
        };
        let file = self.owner.item.file;
        let range = match ptr {
            Some(ptr) => ptr.range,
            None => crate::diagnostics::item_name_range(cx, self.owner.item)?,
        };
        Ok(marlin_lang_diag::Diagnostic::error(
            marlin_lang_diag::Code(self.code),
            self.message.clone(),
        )
        .at(marlin_lang_diag::Span::new(file, range), ""))
    }
}
