//! Typed views over the syntax tree.
//!
//! An AST node is a [`SyntaxNode`] of a known kind; accessors return `Option` because the tree
//! may be incomplete. Only the shape is checked here; meaning is the business of `hir_def`, which
//! grows these accessors as it needs them.

use crate::SyntaxKind::{self, *};
use crate::tree::{SyntaxNode, SyntaxToken};

pub trait AstNode: Sized {
    fn can_cast(kind: SyntaxKind) -> bool;
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;
}

macro_rules! ast_nodes {
    ($($(#[$meta:meta])* $name:ident = $kind:ident,)*) => {$(
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub struct $name(SyntaxNode);

        impl AstNode for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                kind == $kind
            }
            fn cast(node: SyntaxNode) -> Option<Self> {
                Self::can_cast(node.kind()).then(|| Self(node))
            }
            fn syntax(&self) -> &SyntaxNode {
                &self.0
            }
        }
    )*};
}

macro_rules! ast_enums {
    ($($(#[$meta:meta])* $name:ident { $($variant:ident),* $(,)? })*) => {$(
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant($variant),)*
        }

        impl AstNode for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                $($variant::can_cast(kind))||*
            }
            fn cast(node: SyntaxNode) -> Option<Self> {
                $(if $variant::can_cast(node.kind()) {
                    return Some(Self::$variant($variant(node)));
                })*
                None
            }
            fn syntax(&self) -> &SyntaxNode {
                match self {
                    $(Self::$variant(n) => n.syntax(),)*
                }
            }
        }
    )*};
}

ast_nodes! {
    SourceFile = SOURCE_FILE,
    NamespaceDecl = NAMESPACE_DECL,
    UsingDirective = USING_DIRECTIVE,
    UsingAlias = USING_ALIAS,
    AttributeList = ATTRIBUTE_LIST,
    Attribute = ATTRIBUTE,
    Modifiers = MODIFIERS,
    StructDecl = STRUCT_DECL,
    ClassDecl = CLASS_DECL,
    RecordDecl = RECORD_DECL,
    InterfaceDecl = INTERFACE_DECL,
    EnumDecl = ENUM_DECL,
    EnumMemberList = ENUM_MEMBER_LIST,
    EnumMember = ENUM_MEMBER,
    ExtensionDecl = EXTENSION_DECL,
    MemberList = MEMBER_LIST,
    FunctionDecl = FUNCTION_DECL,
    ConstructorDecl = CONSTRUCTOR_DECL,
    ConstructorInitializer = CONSTRUCTOR_INITIALIZER,
    OperatorDecl = OPERATOR_DECL,
    ConversionDecl = CONVERSION_DECL,
    PropertyDecl = PROPERTY_DECL,
    IndexerDecl = INDEXER_DECL,
    AccessorList = ACCESSOR_LIST,
    Accessor = ACCESSOR,
    FieldDecl = FIELD_DECL,
    VariableDeclarator = VARIABLE_DECLARATOR,
    AssocTypeDecl = ASSOC_TYPE_DECL,
    TypeParamList = TYPE_PARAM_LIST,
    TypeParam = TYPE_PARAM,
    BaseList = BASE_LIST,
    WhereClause = WHERE_CLAUSE,
    ParamList = PARAM_LIST,
    Param = PARAM,
    ExprBody = EXPR_BODY,
    Name = NAME,
    NameRef = NAME_REF,

    PredefinedType = PREDEFINED_TYPE,
    PathType = PATH_TYPE,
    Path = PATH,
    PathSegment = PATH_SEGMENT,
    GenericArgList = GENERIC_ARG_LIST,
    TypeArg = TYPE_ARG,
    ConstArg = CONST_ARG,
    ArrayType = ARRAY_TYPE,
    TupleType = TUPLE_TYPE,
    TupleTypeElement = TUPLE_TYPE_ELEMENT,

    Block = BLOCK,
    LocalDeclStmt = LOCAL_DECL_STMT,
    ExprStmt = EXPR_STMT,
    IfStmt = IF_STMT,
    ElseClause = ELSE_CLAUSE,
    ForStmt = FOR_STMT,
    ForInit = FOR_INIT,
    ForUpdate = FOR_UPDATE,
    ForeachStmt = FOREACH_STMT,
    WhileStmt = WHILE_STMT,
    DoStmt = DO_STMT,
    SwitchStmt = SWITCH_STMT,
    SwitchSection = SWITCH_SECTION,
    CaseLabel = CASE_LABEL,
    DefaultLabel = DEFAULT_LABEL,
    BreakStmt = BREAK_STMT,
    ContinueStmt = CONTINUE_STMT,
    ReturnStmt = RETURN_STMT,
    EmptyStmt = EMPTY_STMT,

    Literal = LITERAL,
    InterpolatedString = INTERPOLATED_STRING,
    Interpolation = INTERPOLATION,
    NameExpr = NAME_EXPR,
    TypeExpr = TYPE_EXPR,
    MemberExpr = MEMBER_EXPR,
    CallExpr = CALL_EXPR,
    IndexExpr = INDEX_EXPR,
    ArgList = ARG_LIST,
    BracketArgList = BRACKET_ARG_LIST,
    Arg = ARG,
    NameColon = NAME_COLON,
    ParenExpr = PAREN_EXPR,
    TupleExpr = TUPLE_EXPR,
    PrefixExpr = PREFIX_EXPR,
    PostfixExpr = POSTFIX_EXPR,
    BinExpr = BIN_EXPR,
    ConditionalExpr = CONDITIONAL_EXPR,
    CastExpr = CAST_EXPR,
    RangeExpr = RANGE_EXPR,
    IsExpr = IS_EXPR,
    AsExpr = AS_EXPR,
    SwitchExpr = SWITCH_EXPR,
    SwitchArm = SWITCH_ARM,
    WhenClause = WHEN_CLAUSE,
    WithExpr = WITH_EXPR,
    ObjectInitExpr = OBJECT_INIT_EXPR,
    Initializer = INITIALIZER,
    FieldInit = FIELD_INIT,
    DefaultExpr = DEFAULT_EXPR,
    ThisExpr = THIS_EXPR,
    BaseExpr = BASE_EXPR,
    NewExpr = NEW_EXPR,
    TypeofExpr = TYPEOF_EXPR,
    SizeofExpr = SIZEOF_EXPR,
    ThrowExpr = THROW_EXPR,
    CollectionExpr = COLLECTION_EXPR,
    LambdaExpr = LAMBDA_EXPR,
    LambdaParamList = LAMBDA_PARAM_LIST,
    DeclExpr = DECL_EXPR,
    TupleDesignation = TUPLE_DESIGNATION,

    ConstantPattern = CONSTANT_PATTERN,
    RelationalPattern = RELATIONAL_PATTERN,
    DiscardPattern = DISCARD_PATTERN,
    VarPattern = VAR_PATTERN,
    BinaryPattern = BINARY_PATTERN,
    NotPattern = NOT_PATTERN,
    ParenPattern = PAREN_PATTERN,
    PropertyPattern = PROPERTY_PATTERN,
    PositionalPattern = POSITIONAL_PATTERN,
    Subpattern = SUBPATTERN,
}

ast_enums! {
    /// A declaration at namespace level or inside a type.
    Member {
        StructDecl, ClassDecl, RecordDecl, InterfaceDecl, EnumDecl, ExtensionDecl,
        FunctionDecl, ConstructorDecl, OperatorDecl, ConversionDecl, PropertyDecl,
        IndexerDecl, FieldDecl, AssocTypeDecl, NamespaceDecl,
    }
    Type { PredefinedType, PathType, ArrayType, TupleType }
    Stmt {
        Block, LocalDeclStmt, ExprStmt, IfStmt, ForStmt, ForeachStmt, WhileStmt, DoStmt,
        SwitchStmt, BreakStmt, ContinueStmt, ReturnStmt, EmptyStmt, FunctionDecl,
    }
    Expr {
        Literal, InterpolatedString, NameExpr, TypeExpr, MemberExpr, CallExpr, IndexExpr,
        ParenExpr, TupleExpr, PrefixExpr, PostfixExpr, BinExpr, ConditionalExpr, CastExpr,
        RangeExpr, IsExpr, AsExpr, SwitchExpr, WithExpr, ObjectInitExpr, DefaultExpr, ThisExpr,
        BaseExpr, NewExpr, TypeofExpr, SizeofExpr, ThrowExpr, CollectionExpr, LambdaExpr,
        DeclExpr,
    }
    Pattern {
        ConstantPattern, RelationalPattern, DiscardPattern, VarPattern, BinaryPattern,
        NotPattern, ParenPattern, PropertyPattern, PositionalPattern,
    }
}

// ---- helpers ----

fn child<N: AstNode>(node: &SyntaxNode) -> Option<N> {
    node.children().find_map(N::cast)
}

fn children<'a, N: AstNode + 'a>(node: &'a SyntaxNode) -> impl Iterator<Item = N> + 'a {
    node.children().filter_map(N::cast)
}

fn token(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    node.child_tokens().find(|t| t.kind() == kind)
}

// ---- shared accessors ----

pub trait HasName: AstNode {
    fn name(&self) -> Option<Name> {
        child(self.syntax())
    }
}

pub trait HasAttributes: AstNode {
    fn attribute_lists(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(self.syntax())
    }
}

pub trait HasModifiers: AstNode {
    fn modifiers(&self) -> Option<Modifiers> {
        child(self.syntax())
    }

    fn has_modifier(&self, kind: SyntaxKind) -> bool {
        self.modifiers()
            .is_some_and(|m| m.syntax().child_tokens().any(|t| t.kind() == kind))
    }
}

pub trait HasGenerics: AstNode {
    fn type_param_list(&self) -> Option<TypeParamList> {
        child(self.syntax())
    }

    fn where_clauses(&self) -> impl Iterator<Item = WhereClause> + '_ {
        children(self.syntax())
    }
}

macro_rules! impl_traits {
    ($trait:ident: $($node:ident),*) => { $(impl $trait for $node {})* };
}

impl_traits!(HasName: StructDecl, ClassDecl, RecordDecl, InterfaceDecl, EnumDecl, EnumMember,
    FunctionDecl, ConstructorDecl, PropertyDecl, VariableDeclarator, AssocTypeDecl, TypeParam,
    Param, UsingAlias, TupleTypeElement, VarPattern);
impl_traits!(HasAttributes: StructDecl, ClassDecl, RecordDecl, InterfaceDecl, EnumDecl,
    EnumMember, ExtensionDecl, FunctionDecl, ConstructorDecl, OperatorDecl, ConversionDecl,
    PropertyDecl, IndexerDecl, FieldDecl, AssocTypeDecl, TypeParam, Param, Accessor,
    LocalDeclStmt);
impl_traits!(HasModifiers: StructDecl, ClassDecl, RecordDecl, InterfaceDecl, EnumDecl,
    ExtensionDecl, FunctionDecl, ConstructorDecl, OperatorDecl, ConversionDecl, PropertyDecl,
    IndexerDecl, FieldDecl, AssocTypeDecl, Param, Accessor, LocalDeclStmt);
impl_traits!(HasGenerics: StructDecl, ClassDecl, RecordDecl, InterfaceDecl, ExtensionDecl,
    FunctionDecl);

impl Name {
    pub fn text(&self) -> String {
        self.0
            .first_token()
            .map(|t| t.text().to_string())
            .unwrap_or_default()
    }
}

impl NameRef {
    pub fn text(&self) -> String {
        self.0
            .first_token()
            .map(|t| t.text().to_string())
            .unwrap_or_default()
    }
}

impl SourceFile {
    pub fn usings(&self) -> impl Iterator<Item = UsingDirective> + '_ {
        children(&self.0)
    }

    /// The file-scoped namespace, if any.
    pub fn namespace(&self) -> Option<NamespaceDecl> {
        child(&self.0)
    }

    pub fn members(&self) -> impl Iterator<Item = Member> + '_ {
        children::<Member>(&self.0).filter(|m| !matches!(m, Member::NamespaceDecl(_)))
    }
}

impl NamespaceDecl {
    pub fn path(&self) -> Option<Path> {
        child(&self.0)
    }

    /// The members of a block-scoped namespace.
    pub fn member_list(&self) -> Option<MemberList> {
        child(&self.0)
    }
}

impl MemberList {
    pub fn members(&self) -> impl Iterator<Item = Member> + '_ {
        children(&self.0)
    }
}

impl Path {
    pub fn segments(&self) -> impl Iterator<Item = PathSegment> + '_ {
        children(&self.0)
    }

    /// Whether the path starts with `global::`.
    pub fn is_global(&self) -> bool {
        token(&self.0, GLOBAL_KW).is_some()
    }
}

impl PathSegment {
    pub fn name_ref(&self) -> Option<NameRef> {
        child(&self.0)
    }

    pub fn generic_args(&self) -> Option<GenericArgList> {
        child(&self.0)
    }
}

macro_rules! member_list_accessor {
    ($($node:ident),*) => {$(
        impl $node {
            pub fn member_list(&self) -> Option<MemberList> {
                child(&self.0)
            }

            pub fn base_list(&self) -> Option<BaseList> {
                child(&self.0)
            }
        }
    )*};
}

member_list_accessor!(
    StructDecl,
    ClassDecl,
    RecordDecl,
    InterfaceDecl,
    ExtensionDecl
);

impl BaseList {
    pub fn types(&self) -> impl Iterator<Item = Type> + '_ {
        children(&self.0)
    }
}

impl FunctionDecl {
    pub fn ret_type(&self) -> Option<Type> {
        child(&self.0)
    }

    pub fn param_list(&self) -> Option<ParamList> {
        child(&self.0)
    }

    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }

    pub fn expr_body(&self) -> Option<ExprBody> {
        child(&self.0)
    }
}

impl ParamList {
    pub fn params(&self) -> impl Iterator<Item = Param> + '_ {
        children(&self.0)
    }
}

impl Param {
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl FieldDecl {
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }

    pub fn declarators(&self) -> impl Iterator<Item = VariableDeclarator> + '_ {
        children(&self.0)
    }
}

impl VariableDeclarator {
    pub fn initializer(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl Block {
    pub fn statements(&self) -> impl Iterator<Item = Stmt> + '_ {
        children(&self.0)
    }
}

impl TypeParam {
    /// The type of a constant generic parameter (`const int N`).
    pub fn const_type(&self) -> Option<Type> {
        token(&self.0, CONST_KW).and_then(|_| child(&self.0))
    }
}

impl IfStmt {
    /// Whether this is a `const if`.
    pub fn is_const(&self) -> bool {
        token(&self.0, CONST_KW).is_some()
    }

    pub fn condition(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl Literal {
    pub fn token(&self) -> Option<SyntaxToken> {
        self.0.first_token()
    }
}

impl BinExpr {
    pub fn lhs(&self) -> Option<Expr> {
        children(&self.0).next()
    }

    pub fn rhs(&self) -> Option<Expr> {
        children(&self.0).nth(1)
    }

    /// The operator token (a glued `>>` counts as one).
    pub fn op(&self) -> Option<SyntaxToken> {
        self.0.child_tokens().find(|t| !t.kind().is_trivia())
    }
}

impl UsingDirective {
    /// `global using ...`
    pub fn is_global(&self) -> bool {
        token(&self.0, GLOBAL_KW).is_some()
    }

    /// `using static ...`
    pub fn is_static(&self) -> bool {
        token(&self.0, STATIC_KW).is_some()
    }

    /// `using Name = ...`
    pub fn alias(&self) -> Option<UsingAlias> {
        child(&self.0)
    }

    /// The imported namespace or type: the path, or for an alias the path of its target type.
    pub fn path(&self) -> Option<Path> {
        child::<Path>(&self.0).or_else(|| match child::<Type>(&self.0)? {
            Type::PathType(t) => t.path(),
            _ => None,
        })
    }
}

impl PathType {
    pub fn path(&self) -> Option<Path> {
        child(&self.0)
    }
}

impl GenericArgList {
    /// How many arguments are written.
    pub fn count(&self) -> usize {
        self.0
            .children()
            .filter(|n| matches!(n.kind(), TYPE_ARG | CONST_ARG))
            .count()
    }
}

impl TypeParamList {
    pub fn params(&self) -> impl Iterator<Item = TypeParam> + '_ {
        children(&self.0)
    }
}

impl EnumDecl {
    pub fn members(&self) -> impl Iterator<Item = EnumMember> + '_ {
        child::<EnumMemberList>(&self.0)
            .into_iter()
            .flat_map(|list| {
                list.0
                    .children()
                    .filter_map(EnumMember::cast)
                    .collect::<Vec<_>>()
            })
    }
}

impl LocalDeclStmt {
    pub fn declarators(&self) -> impl Iterator<Item = VariableDeclarator> + '_ {
        children(&self.0)
    }
}

impl Modifiers {
    /// The modifier keywords, in source order.
    pub fn kinds(&self) -> impl Iterator<Item = SyntaxKind> + '_ {
        self.0
            .child_tokens()
            .map(|t| t.kind())
            .filter(|k| !k.is_trivia())
    }
}
