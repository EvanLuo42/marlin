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
