//! The HIR: expressions, statements, patterns, type references and local bindings, in arenas.
//!
//! The same [`ExprStore`] holds a function body ([`crate::Body`]) and the types and expressions
//! of a declaration's signature ([`crate::Signature`]); constant generic arguments and array
//! lengths are ordinary expressions in it. The HIR keeps C#'s constructs nearly as written:
//! only pure syntax (parentheses, expression bodies) is gone. Names stay unresolved paths;
//! `hir_ty` resolves them with the [`Resolver`](crate::Resolver) and the local scopes.

use std::collections::HashMap;

use marlin_lang_arena::{Arena, ArenaMap, Idx};

use crate::Name;
use crate::ast_id::AstPtr;

pub type ExprId = Idx<Expr>;
pub type StmtId = Idx<Stmt>;
pub type PatId = Idx<Pat>;
pub type TypeRefId = Idx<TypeRef>;
pub type BindingId = Idx<Binding>;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ExprStore {
    pub exprs: Arena<Expr>,
    pub stmts: Arena<Stmt>,
    pub pats: Arena<Pat>,
    pub types: Arena<TypeRef>,
    pub bindings: Arena<Binding>,
}

/// Where each HIR node came from in the current syntax tree, and back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreSourceMap {
    pub exprs: ArenaMap<Expr, AstPtr>,
    pub stmts: ArenaMap<Stmt, AstPtr>,
    pub pats: ArenaMap<Pat, AstPtr>,
    pub types: ArenaMap<TypeRef, AstPtr>,
    /// The name that declares each binding.
    pub bindings: ArenaMap<Binding, AstPtr>,
    pub expr_of: HashMap<AstPtr, ExprId>,
}

impl StoreSourceMap {
    pub fn expr_for(&self, ptr: &AstPtr) -> Option<ExprId> {
        self.expr_of.get(ptr).copied()
    }
}

/// A type as written.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TypeRef {
    Missing,
    Predefined(Predefined),
    Path(HirPath),
    /// `T[len]`; `len` is `None` when missing.
    Array {
        element: TypeRefId,
        len: Option<ExprId>,
    },
    Tuple(Vec<(TypeRefId, Option<Name>)>),
}

/// The built-in type keywords.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Predefined {
    Bool,
    Byte,
    Char,
    Decimal,
    Double,
    Float,
    Half,
    Int,
    Long,
    Object,
    Sbyte,
    Short,
    String,
    Uint,
    Ulong,
    Ushort,
    Void,
}

impl Predefined {
    pub fn keyword(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Byte => "byte",
            Self::Char => "char",
            Self::Decimal => "decimal",
            Self::Double => "double",
            Self::Float => "float",
            Self::Half => "half",
            Self::Int => "int",
            Self::Long => "long",
            Self::Object => "object",
            Self::Sbyte => "sbyte",
            Self::Short => "short",
            Self::String => "string",
            Self::Uint => "uint",
            Self::Ulong => "ulong",
            Self::Ushort => "ushort",
            Self::Void => "void",
        }
    }
}

/// A path with its type arguments: `global::A.B<T, 3>.C`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HirPath {
    pub global: bool,
    pub segments: Vec<HirSegment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HirSegment {
    pub name: Name,
    /// `None` when no `<...>` is written.
    pub args: Option<Vec<GenericArg>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GenericArg {
    Type(TypeRefId),
    Const(ExprId),
}

impl HirPath {
    pub fn single(name: Name) -> Self {
        Self {
            global: false,
            segments: vec![HirSegment { name, args: None }],
        }
    }

    /// The path the [`Resolver`](crate::Resolver) takes: names and type-argument counts.
    pub fn to_def_path(&self) -> crate::Path {
        crate::Path {
            global: self.global,
            segments: self
                .segments
                .iter()
                .map(|s| crate::PathSegment {
                    name: s.name.clone(),
                    arity: s.args.as_ref().map_or(0, |a| a.len() as u32),
                })
                .collect(),
        }
    }

    /// A lone name without type arguments, such as a local's.
    pub fn as_name(&self) -> Option<&Name> {
        match self.segments.as_slice() {
            [segment] if !self.global && segment.args.is_none() => Some(&segment.name),
            _ => None,
        }
    }
}

/// `[Name(args)]`
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Attribute {
    pub path: HirPath,
    pub args: Vec<Arg>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Arg {
    /// `lod: 2`
    pub name: Option<Name>,
    pub modifier: Option<RefKind>,
    pub expr: ExprId,
}

/// `in`, `out`, `ref` on arguments and parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefKind {
    In,
    Out,
    Ref,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Literal {
    /// Integer or real, as written (suffix included); its type and value are for `hir_ty`.
    Number(Box<str>),
    Bool(bool),
    Null,
    /// As written, quotes and escapes included.
    String(Box<str>),
    Char(Box<str>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Neg,
    Plus,
    Not,
    BitNot,
    PreInc,
    PreDec,
    PostInc,
    PostDec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinaryOp {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Rem => "%",
            Self::Shl => "<<",
            Self::Shr => ">>",
            Self::UShr => ">>>",
            Self::BitAnd => "&",
            Self::BitOr => "|",
            Self::BitXor => "^",
            Self::And => "&&",
            Self::Or => "||",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum InterpolationPart {
    /// Literal text as written (escapes and `{{` included).
    Text(Box<str>),
    Hole {
        expr: ExprId,
        alignment: Option<ExprId>,
        format: Option<Box<str>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FieldInit {
    pub name: Name,
    pub value: ExprId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SwitchArm {
    pub pat: PatId,
    pub guard: Option<ExprId>,
    pub expr: ExprId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LambdaBody {
    Expr(ExprId),
    Block(StmtId),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LocalParam {
    pub binding: BindingId,
    pub modifier: Option<RefKind>,
    /// `None` for an untyped lambda parameter.
    pub ty: Option<TypeRefId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Expr {
    /// Broken or absent syntax.
    Missing,
    Literal(Literal),
    Interpolated(Vec<InterpolationPart>),
    /// A name or a dotted chain of names: a local, a declaration, or `local.field...`.
    Path(HirPath),
    /// A built-in type keyword used as an expression: `float(x)`, `float.MaxValue`.
    Type(TypeRefId),
    /// `.name` on anything that is not a plain dotted name.
    Member {
        receiver: ExprId,
        name: Name,
        args: Option<Vec<GenericArg>>,
    },
    Call {
        callee: ExprId,
        args: Vec<Arg>,
    },
    Index {
        base: ExprId,
        args: Vec<Arg>,
    },
    Unary {
        op: UnaryOp,
        operand: ExprId,
    },
    Binary {
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    /// `target = value`, or `target op= value`.
    Assign {
        op: Option<BinaryOp>,
        target: ExprId,
        value: ExprId,
    },
    Conditional {
        cond: ExprId,
        then: ExprId,
        otherwise: ExprId,
    },
    Cast {
        ty: TypeRefId,
        expr: ExprId,
    },
    Range {
        start: Option<ExprId>,
        end: Option<ExprId>,
    },
    Is {
        expr: ExprId,
        pat: PatId,
    },
    As {
        expr: ExprId,
        ty: TypeRefId,
    },
    Switch {
        scrutinee: ExprId,
        arms: Vec<SwitchArm>,
    },
    With {
        expr: ExprId,
        inits: Vec<FieldInit>,
    },
    /// `T { A = 1 }`, or target-typed `{ A = 1 }`.
    ObjectInit {
        ty: Option<TypeRefId>,
        inits: Vec<FieldInit>,
    },
    Default(Option<TypeRefId>),
    This,
    Base,
    New {
        ty: Option<TypeRefId>,
        args: Option<Vec<Arg>>,
        inits: Option<Vec<FieldInit>>,
    },
    Typeof(TypeRefId),
    Sizeof(TypeRefId),
    Throw(Option<ExprId>),
    Collection(Vec<ExprId>),
    Tuple(Vec<(Option<Name>, ExprId)>),
    Lambda {
        params: Vec<LocalParam>,
        body: LambdaBody,
    },
    /// `var x` / `float3 n` in an `out` argument or on the left of a deconstruction.
    DeclVar {
        ty: Option<TypeRefId>,
        binding: BindingId,
    },
    /// `var (a, b)`
    DeclTuple(Vec<BindingId>),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LocalFunction {
    pub name: Name,
    pub binding: BindingId,
    pub is_static: bool,
    pub generic_params: Vec<Name>,
    pub params: Vec<LocalParam>,
    pub ret: TypeRefId,
    pub body: Option<LambdaBody>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SwitchSection {
    /// `None` is `default:`.
    pub labels: Vec<Option<(PatId, Option<ExprId>)>>,
    pub body: Vec<StmtId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Stmt {
    Missing,
    Expr(ExprId),
    /// `[attrs] const T a = x, b;` (`ty` is `None` for `var`).
    Local {
        attributes: Vec<Attribute>,
        is_const: bool,
        ty: Option<TypeRefId>,
        declarators: Vec<(BindingId, Option<ExprId>)>,
    },
    Block(Vec<StmtId>),
    If {
        is_const: bool,
        cond: ExprId,
        then: StmtId,
        otherwise: Option<StmtId>,
    },
    For {
        init: Vec<StmtId>,
        cond: Option<ExprId>,
        update: Vec<ExprId>,
        body: StmtId,
    },
    Foreach {
        /// A typed or `var` variable, or `var (a, b)`.
        variable: ExprId,
        iterable: ExprId,
        body: StmtId,
    },
    While {
        cond: ExprId,
        body: StmtId,
    },
    Do {
        body: StmtId,
        cond: ExprId,
    },
    Switch {
        scrutinee: ExprId,
        sections: Vec<SwitchSection>,
    },
    Break,
    Continue,
    Return(Option<ExprId>),
    LocalFunction(Box<LocalFunction>),
    Empty,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Pat {
    Missing,
    Constant(ExprId),
    Relational {
        op: BinaryOp,
        value: ExprId,
    },
    Discard,
    Var(BindingId),
    And(PatId, PatId),
    Or(PatId, PatId),
    Not(PatId),
    /// `{ A: p, B.C: q }`: member paths and their patterns.
    Property(Vec<(Option<Vec<Name>>, PatId)>),
    Positional(Vec<PatId>),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Binding {
    pub name: Name,
    pub kind: BindingKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BindingKind {
    /// The `index`th parameter of the body's owner.
    Param(u32),
    /// The receiver of an extension block (`extension(float3 c)`).
    Receiver,
    /// The implicit `value` of a setter.
    SetterValue,
    Local {
        is_const: bool,
    },
    /// `var x` in a pattern, `out var x`, deconstruction, `foreach`.
    Declared,
    LambdaParam,
    LocalFunction,
    LocalFunctionParam,
}

/// A node of a body, for walking it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    Expr(ExprId),
    Stmt(StmtId),
    Pat(PatId),
}

impl ExprStore {
    /// The direct children of `node` that are expressions, statements or patterns, in source
    /// order. Types and attributes are left out.
    pub fn children(&self, node: Node) -> Vec<Node> {
        let mut out = Vec::new();
        let args = |out: &mut Vec<Node>, args: &[Arg]| {
            out.extend(args.iter().map(|a| Node::Expr(a.expr)));
        };
        let lambda = |out: &mut Vec<Node>, body: LambdaBody| {
            out.push(match body {
                LambdaBody::Expr(e) => Node::Expr(e),
                LambdaBody::Block(s) => Node::Stmt(s),
            });
        };
        match node {
            Node::Expr(e) => match &self.exprs[e] {
                Expr::Missing
                | Expr::Literal(_)
                | Expr::Path(_)
                | Expr::Type(_)
                | Expr::Default(_)
                | Expr::This
                | Expr::Base
                | Expr::Typeof(_)
                | Expr::Sizeof(_)
                | Expr::DeclVar { .. }
                | Expr::DeclTuple(_)
                | Expr::Throw(None) => {}
                Expr::Interpolated(parts) => {
                    for part in parts {
                        if let InterpolationPart::Hole {
                            expr, alignment, ..
                        } = part
                        {
                            out.push(Node::Expr(*expr));
                            out.extend(alignment.map(Node::Expr));
                        }
                    }
                }
                Expr::Member { receiver, .. } => out.push(Node::Expr(*receiver)),
                Expr::Call {
                    callee: base,
                    args: a,
                }
                | Expr::Index { base, args: a } => {
                    out.push(Node::Expr(*base));
                    args(&mut out, a);
                }
                Expr::Unary { operand, .. } => out.push(Node::Expr(*operand)),
                Expr::Binary { lhs, rhs, .. } => {
                    out.extend([Node::Expr(*lhs), Node::Expr(*rhs)]);
                }
                Expr::Assign { target, value, .. } => {
                    out.extend([Node::Expr(*target), Node::Expr(*value)]);
                }
                Expr::Conditional {
                    cond,
                    then,
                    otherwise,
                } => out.extend([cond, then, otherwise].map(|x| Node::Expr(*x))),
                Expr::Cast { expr, .. } | Expr::As { expr, .. } | Expr::Throw(Some(expr)) => {
                    out.push(Node::Expr(*expr));
                }
                Expr::Range { start, end } => {
                    out.extend(start.iter().chain(end).map(|x| Node::Expr(*x)));
                }
                Expr::Is { expr, pat } => out.extend([Node::Expr(*expr), Node::Pat(*pat)]),
                Expr::Switch { scrutinee, arms } => {
                    out.push(Node::Expr(*scrutinee));
                    for arm in arms {
                        out.push(Node::Pat(arm.pat));
                        out.extend(arm.guard.map(Node::Expr));
                        out.push(Node::Expr(arm.expr));
                    }
                }
                Expr::With { expr, inits } => {
                    out.push(Node::Expr(*expr));
                    out.extend(inits.iter().map(|i| Node::Expr(i.value)));
                }
                Expr::ObjectInit { inits, .. } => {
                    out.extend(inits.iter().map(|i| Node::Expr(i.value)));
                }
                Expr::New { args: a, inits, .. } => {
                    if let Some(a) = a {
                        args(&mut out, a);
                    }
                    out.extend(inits.iter().flatten().map(|i| Node::Expr(i.value)));
                }
                Expr::Collection(items) => out.extend(items.iter().map(|x| Node::Expr(*x))),
                Expr::Tuple(items) => out.extend(items.iter().map(|(_, x)| Node::Expr(*x))),
                Expr::Lambda { body, .. } => lambda(&mut out, *body),
            },
            Node::Stmt(s) => match &self.stmts[s] {
                Stmt::Missing | Stmt::Empty | Stmt::Break | Stmt::Continue => {}
                Stmt::Expr(e) => out.push(Node::Expr(*e)),
                Stmt::Local { declarators, .. } => {
                    out.extend(declarators.iter().filter_map(|(_, i)| i.map(Node::Expr)));
                }
                Stmt::Block(stmts) => out.extend(stmts.iter().map(|s| Node::Stmt(*s))),
                Stmt::If {
                    cond,
                    then,
                    otherwise,
                    ..
                } => {
                    out.extend([Node::Expr(*cond), Node::Stmt(*then)]);
                    out.extend(otherwise.map(Node::Stmt));
                }
                Stmt::For {
                    init,
                    cond,
                    update,
                    body,
                } => {
                    out.extend(init.iter().map(|s| Node::Stmt(*s)));
                    out.extend(cond.map(Node::Expr));
                    out.extend(update.iter().map(|e| Node::Expr(*e)));
                    out.push(Node::Stmt(*body));
                }
                Stmt::Foreach {
                    variable,
                    iterable,
                    body,
                } => out.extend([
                    Node::Expr(*variable),
                    Node::Expr(*iterable),
                    Node::Stmt(*body),
                ]),
                Stmt::While { cond, body } => out.extend([Node::Expr(*cond), Node::Stmt(*body)]),
                Stmt::Do { body, cond } => out.extend([Node::Stmt(*body), Node::Expr(*cond)]),
                Stmt::Switch {
                    scrutinee,
                    sections,
                } => {
                    out.push(Node::Expr(*scrutinee));
                    for section in sections {
                        for (pat, guard) in section.labels.iter().flatten() {
                            out.push(Node::Pat(*pat));
                            out.extend(guard.map(Node::Expr));
                        }
                        out.extend(section.body.iter().map(|s| Node::Stmt(*s)));
                    }
                }
                Stmt::Return(value) => out.extend(value.map(Node::Expr)),
                Stmt::LocalFunction(f) => {
                    if let Some(body) = f.body {
                        lambda(&mut out, body);
                    }
                }
            },
            Node::Pat(p) => match &self.pats[p] {
                Pat::Missing | Pat::Discard | Pat::Var(_) => {}
                Pat::Constant(e) | Pat::Relational { value: e, .. } => out.push(Node::Expr(*e)),
                Pat::And(a, b) | Pat::Or(a, b) => out.extend([Node::Pat(*a), Node::Pat(*b)]),
                Pat::Not(a) => out.push(Node::Pat(*a)),
                Pat::Property(items) => out.extend(items.iter().map(|(_, p)| Node::Pat(*p))),
                Pat::Positional(items) => out.extend(items.iter().map(|p| Node::Pat(*p))),
            },
        }
        out
    }

    /// `node` and everything under it, depth first.
    pub fn descendants(&self, node: Node) -> Vec<Node> {
        let mut out = Vec::new();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            out.push(n);
            let mut children = self.children(n);
            children.reverse();
            stack.extend(children);
        }
        out
    }
}
