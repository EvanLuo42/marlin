//! Syntax → HIR.

use marlin_front_syntax::ast::{self, AstNode};
use marlin_front_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use crate::Name;
use crate::ast_id::AstPtr;
use crate::store::*;

use SyntaxKind::*;

pub(crate) struct Lowerer {
    pub(crate) store: ExprStore,
    pub(crate) map: StoreSourceMap,
}

pub(crate) fn is_expr(kind: SyntaxKind) -> bool {
    ast::Expr::can_cast(kind) || kind == PAREN_EXPR
}

pub(crate) fn is_type(kind: SyntaxKind) -> bool {
    ast::Type::can_cast(kind)
}

fn is_pat(kind: SyntaxKind) -> bool {
    ast::Pattern::can_cast(kind)
}

fn is_stmt(kind: SyntaxKind) -> bool {
    ast::Stmt::can_cast(kind)
}

pub(crate) fn child(node: &SyntaxNode, f: fn(SyntaxKind) -> bool) -> Option<SyntaxNode> {
    node.children().find(|c| f(c.kind()))
}

fn children(node: &SyntaxNode, f: fn(SyntaxKind) -> bool) -> Vec<SyntaxNode> {
    node.children().filter(|c| f(c.kind())).collect()
}

pub(crate) fn child_of_kind(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    node.children().find(|c| c.kind() == kind)
}

fn token(node: &SyntaxNode, f: impl Fn(SyntaxKind) -> bool) -> Option<SyntaxToken> {
    node.child_tokens().find(|t| f(t.kind()))
}

pub(crate) fn name_of(node: &SyntaxNode) -> Option<Name> {
    child_of_kind(node, NAME)
        .or_else(|| child_of_kind(node, NAME_REF))
        .and_then(|n| n.first_token())
        .map(|t| Name::new(t.text()))
}

fn ref_kind(node: &SyntaxNode) -> Option<RefKind> {
    let in_modifiers = child_of_kind(node, MODIFIERS);
    let tokens = node.child_tokens().chain(
        in_modifiers
            .iter()
            .flat_map(|m| m.child_tokens().collect::<Vec<_>>()),
    );
    for t in tokens {
        match t.kind() {
            IN_KW => return Some(RefKind::In),
            OUT_KW => return Some(RefKind::Out),
            REF_KW => return Some(RefKind::Ref),
            _ => {}
        }
    }
    None
}

fn binary_op(kind: SyntaxKind) -> Option<BinaryOp> {
    Some(match kind {
        PLUS | PLUS_EQ => BinaryOp::Add,
        MINUS | MINUS_EQ => BinaryOp::Sub,
        STAR | STAR_EQ => BinaryOp::Mul,
        SLASH | SLASH_EQ => BinaryOp::Div,
        PERCENT | PERCENT_EQ => BinaryOp::Rem,
        SHL | SHL_EQ => BinaryOp::Shl,
        SHR | SHR_EQ => BinaryOp::Shr,
        USHR | USHR_EQ => BinaryOp::UShr,
        AMP | AMP_EQ => BinaryOp::BitAnd,
        PIPE | PIPE_EQ => BinaryOp::BitOr,
        CARET | CARET_EQ => BinaryOp::BitXor,
        AMP2 => BinaryOp::And,
        PIPE2 => BinaryOp::Or,
        EQ2 => BinaryOp::Eq,
        NEQ => BinaryOp::Ne,
        LT => BinaryOp::Lt,
        LTEQ => BinaryOp::Le,
        GT => BinaryOp::Gt,
        GTEQ => BinaryOp::Ge,
        _ => return None,
    })
}

fn is_assign_op(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        EQ | PLUS_EQ
            | MINUS_EQ
            | STAR_EQ
            | SLASH_EQ
            | PERCENT_EQ
            | SHL_EQ
            | SHR_EQ
            | USHR_EQ
            | AMP_EQ
            | PIPE_EQ
            | CARET_EQ
    )
}

fn predefined(kind: SyntaxKind) -> Option<Predefined> {
    Some(match kind {
        BOOL_KW => Predefined::Bool,
        BYTE_KW => Predefined::Byte,
        CHAR_KW => Predefined::Char,
        DECIMAL_KW => Predefined::Decimal,
        DOUBLE_KW => Predefined::Double,
        FLOAT_KW => Predefined::Float,
        HALF_KW => Predefined::Half,
        INT_KW => Predefined::Int,
        LONG_KW => Predefined::Long,
        OBJECT_KW => Predefined::Object,
        SBYTE_KW => Predefined::Sbyte,
        SHORT_KW => Predefined::Short,
        STRING_KW => Predefined::String,
        UINT_KW => Predefined::Uint,
        ULONG_KW => Predefined::Ulong,
        USHORT_KW => Predefined::Ushort,
        VOID_KW => Predefined::Void,
        _ => return None,
    })
}

/// Whether a type node is the contextual `var`.
pub(crate) fn is_var(node: &SyntaxNode) -> bool {
    node.kind() == PATH_TYPE && node.text().trim() == "var"
}

impl Lowerer {
    pub(crate) fn new() -> Self {
        Self {
            store: ExprStore::default(),
            map: StoreSourceMap::default(),
        }
    }

    fn alloc_expr(&mut self, expr: Expr, node: Option<&SyntaxNode>) -> ExprId {
        let id = self.store.exprs.alloc(expr);
        if let Some(node) = node {
            let ptr = AstPtr::new(node);
            self.map.exprs.insert(id, ptr);
            self.map.expr_of.insert(ptr, id);
        }
        id
    }

    fn alloc_stmt(&mut self, stmt: Stmt, node: &SyntaxNode) -> StmtId {
        let id = self.store.stmts.alloc(stmt);
        self.map.stmts.insert(id, AstPtr::new(node));
        id
    }

    fn alloc_pat(&mut self, pat: Pat, node: Option<&SyntaxNode>) -> PatId {
        let id = self.store.pats.alloc(pat);
        if let Some(node) = node {
            self.map.pats.insert(id, AstPtr::new(node));
        }
        id
    }

    fn alloc_type(&mut self, ty: TypeRef, node: Option<&SyntaxNode>) -> TypeRefId {
        let id = self.store.types.alloc(ty);
        if let Some(node) = node {
            self.map.types.insert(id, AstPtr::new(node));
        }
        id
    }

    pub(crate) fn binding(
        &mut self,
        name: Name,
        kind: BindingKind,
        node: Option<&SyntaxNode>,
    ) -> BindingId {
        let id = self.store.bindings.alloc(Binding { name, kind });
        if let Some(node) = node {
            self.map.bindings.insert(id, AstPtr::new(node));
        }
        id
    }

    /// A binding for the `NAME` child of `node`, if it has one.
    fn named_binding(&mut self, node: &SyntaxNode, kind: BindingKind) -> Option<BindingId> {
        let name_node = child_of_kind(node, NAME)?;
        let name = Name::new(name_node.first_token()?.text());
        Some(self.binding(name, kind, Some(&name_node)))
    }

    // ---- types ----

    pub(crate) fn type_opt(&mut self, node: Option<SyntaxNode>) -> TypeRefId {
        match node {
            Some(node) => self.ty(&node),
            None => self.alloc_type(TypeRef::Missing, None),
        }
    }

    pub(crate) fn ty(&mut self, node: &SyntaxNode) -> TypeRefId {
        let ty = match node.kind() {
            PREDEFINED_TYPE => node
                .first_token()
                .and_then(|t| predefined(t.kind()))
                .map_or(TypeRef::Missing, TypeRef::Predefined),
            PATH_TYPE => match child_of_kind(node, PATH) {
                Some(path) => TypeRef::Path(self.path(&path)),
                None => TypeRef::Missing,
            },
            ARRAY_TYPE => {
                let element = self.type_opt(child(node, is_type));
                let len = child(node, is_expr).map(|e| self.expr(&e));
                TypeRef::Array { element, len }
            }
            TUPLE_TYPE => TypeRef::Tuple(
                node.children()
                    .filter(|c| c.kind() == TUPLE_TYPE_ELEMENT)
                    .map(|e| (self.type_opt(child(&e, is_type)), name_of(&e)))
                    .collect(),
            ),
            _ => TypeRef::Missing,
        };
        self.alloc_type(ty, Some(node))
    }

    pub(crate) fn path(&mut self, node: &SyntaxNode) -> HirPath {
        let global = token(node, |k| k == GLOBAL_KW).is_some();
        let segments = node
            .children()
            .filter(|c| c.kind() == PATH_SEGMENT)
            .map(|s| HirSegment {
                name: name_of(&s).unwrap_or_else(|| Name::new("<missing>")),
                args: child_of_kind(&s, GENERIC_ARG_LIST).map(|l| self.generic_args(&l)),
            })
            .collect();
        HirPath { global, segments }
    }

    fn generic_args(&mut self, list: &SyntaxNode) -> Vec<GenericArg> {
        list.children()
            .filter_map(|a| match a.kind() {
                TYPE_ARG => Some(GenericArg::Type(self.type_opt(child(&a, is_type)))),
                CONST_ARG => Some(GenericArg::Const(self.expr_opt(child(&a, is_expr)))),
                _ => None,
            })
            .collect()
    }

    /// The type an expression names, for `Light { ... }`: a name, a dotted name, or a built-in
    /// type keyword.
    fn expr_as_type(&mut self, node: &SyntaxNode) -> TypeRefId {
        let path = self.expr_path(node);
        match path {
            Some(path) => self.alloc_type(TypeRef::Path(path), Some(node)),
            None if node.kind() == TYPE_EXPR => self.type_opt(child(node, is_type)),
            None => self.alloc_type(TypeRef::Missing, Some(node)),
        }
    }

    /// A `NAME_EXPR` or a chain of `MEMBER_EXPR`s over one, as a path.
    fn expr_path(&mut self, node: &SyntaxNode) -> Option<HirPath> {
        match node.kind() {
            NAME_EXPR => {
                let name = name_of(node)?;
                let args = child_of_kind(node, GENERIC_ARG_LIST).map(|l| self.generic_args(&l));
                Some(HirPath {
                    global: false,
                    segments: vec![HirSegment { name, args }],
                })
            }
            MEMBER_EXPR => {
                let receiver = child(node, is_expr)?;
                if !matches!(receiver.kind(), NAME_EXPR | MEMBER_EXPR) {
                    return None;
                }
                let mut path = self.expr_path(&receiver)?;
                let name = name_of(node)?;
                let args = child_of_kind(node, GENERIC_ARG_LIST).map(|l| self.generic_args(&l));
                path.segments.push(HirSegment { name, args });
                Some(path)
            }
            _ => None,
        }
    }

    pub(crate) fn attributes(&mut self, node: &SyntaxNode) -> Vec<Attribute> {
        node.children()
            .filter(|c| c.kind() == ATTRIBUTE_LIST)
            .flat_map(|l| {
                l.children()
                    .filter(|a| a.kind() == ATTRIBUTE)
                    .collect::<Vec<_>>()
            })
            .filter_map(|a| {
                let path = self.path(&child_of_kind(&a, PATH)?);
                let args = child_of_kind(&a, ARG_LIST)
                    .map(|l| self.args(&l))
                    .unwrap_or_default();
                Some(Attribute { path, args })
            })
            .collect()
    }

    // ---- expressions ----

    pub(crate) fn expr_opt(&mut self, node: Option<SyntaxNode>) -> ExprId {
        match node {
            Some(node) => self.expr(&node),
            None => self.alloc_expr(Expr::Missing, None),
        }
    }

    pub(crate) fn expr(&mut self, node: &SyntaxNode) -> ExprId {
        if node.kind() == PAREN_EXPR {
            let inner = self.expr_opt(child(node, is_expr));
            let ptr = AstPtr::new(node);
            self.map.expr_of.insert(ptr, inner);
            return inner;
        }
        if let Some(path) = self.expr_path(node) {
            return self.alloc_expr(Expr::Path(path), Some(node));
        }
        let exprs = |s: &mut Self| -> Vec<ExprId> {
            children(node, is_expr).iter().map(|e| s.expr(e)).collect()
        };
        let expr = match node.kind() {
            LITERAL => match node.first_token() {
                Some(t) => match t.kind() {
                    TRUE_KW => Expr::Literal(Literal::Bool(true)),
                    FALSE_KW => Expr::Literal(Literal::Bool(false)),
                    NULL_KW => Expr::Literal(Literal::Null),
                    STRING => Expr::Literal(Literal::String(t.text().into())),
                    CHAR => Expr::Literal(Literal::Char(t.text().into())),
                    _ => Expr::Literal(Literal::Number(t.text().into())),
                },
                None => Expr::Missing,
            },
            INTERPOLATED_STRING => {
                let mut parts = Vec::new();
                for element in node.children_with_tokens() {
                    match element {
                        marlin_front_syntax::SyntaxElement::Token(t) if t.kind() == INTERP_TEXT => {
                            parts.push(InterpolationPart::Text(t.text().into()));
                        }
                        marlin_front_syntax::SyntaxElement::Node(n)
                            if n.kind() == INTERPOLATION =>
                        {
                            let holes = children(&n, is_expr);
                            let expr = self.expr_opt(holes.first().cloned());
                            let alignment = holes.get(1).map(|a| self.expr(a));
                            let format = token(&n, |k| k == INTERP_FORMAT).map(|t| t.text().into());
                            parts.push(InterpolationPart::Hole {
                                expr,
                                alignment,
                                format,
                            });
                        }
                        _ => {}
                    }
                }
                Expr::Interpolated(parts)
            }
            TYPE_EXPR => Expr::Type(self.type_opt(child(node, is_type))),
            MEMBER_EXPR => Expr::Member {
                receiver: self.expr_opt(child(node, is_expr)),
                name: name_of(node).unwrap_or_else(|| Name::new("<missing>")),
                args: child_of_kind(node, GENERIC_ARG_LIST).map(|l| self.generic_args(&l)),
            },
            CALL_EXPR => Expr::Call {
                callee: self.expr_opt(child(node, is_expr)),
                args: child_of_kind(node, ARG_LIST)
                    .map(|l| self.args(&l))
                    .unwrap_or_default(),
            },
            INDEX_EXPR => Expr::Index {
                base: self.expr_opt(child(node, is_expr)),
                args: child_of_kind(node, BRACKET_ARG_LIST)
                    .map(|l| self.args(&l))
                    .unwrap_or_default(),
            },
            PREFIX_EXPR => {
                let op = match token(node, |k| !k.is_trivia()).map(|t| t.kind()) {
                    Some(MINUS) => UnaryOp::Neg,
                    Some(PLUS) => UnaryOp::Plus,
                    Some(BANG) => UnaryOp::Not,
                    Some(TILDE) => UnaryOp::BitNot,
                    Some(PLUS2) => UnaryOp::PreInc,
                    _ => UnaryOp::PreDec,
                };
                Expr::Unary {
                    op,
                    operand: self.expr_opt(child(node, is_expr)),
                }
            }
            POSTFIX_EXPR => {
                let op = match token(node, |k| matches!(k, PLUS2 | MINUS2)).map(|t| t.kind()) {
                    Some(PLUS2) => UnaryOp::PostInc,
                    _ => UnaryOp::PostDec,
                };
                Expr::Unary {
                    op,
                    operand: self.expr_opt(child(node, is_expr)),
                }
            }
            BIN_EXPR => {
                let operands = exprs(self);
                let lhs = operands.first().copied();
                let rhs = operands.get(1).copied();
                let lhs = lhs.unwrap_or_else(|| self.alloc_expr(Expr::Missing, None));
                let rhs = rhs.unwrap_or_else(|| self.alloc_expr(Expr::Missing, None));
                let op = token(node, |k| !k.is_trivia()).map(|t| t.kind());
                match op {
                    Some(k) if is_assign_op(k) => Expr::Assign {
                        op: binary_op(k),
                        target: lhs,
                        value: rhs,
                    },
                    Some(k) => match binary_op(k) {
                        Some(op) => Expr::Binary { op, lhs, rhs },
                        None => Expr::Missing,
                    },
                    None => Expr::Missing,
                }
            }
            CONDITIONAL_EXPR => {
                let parts = exprs(self);
                let mut get = |i: usize| {
                    parts
                        .get(i)
                        .copied()
                        .unwrap_or_else(|| self.alloc_expr(Expr::Missing, None))
                };
                Expr::Conditional {
                    cond: get(0),
                    then: get(1),
                    otherwise: get(2),
                }
            }
            CAST_EXPR => Expr::Cast {
                ty: self.type_opt(child(node, is_type)),
                expr: self.expr_opt(child(node, is_expr)),
            },
            RANGE_EXPR => {
                let dots = token(node, |k| k == DOT2).map(|t| t.text_range().start());
                let mut start = None;
                let mut end = None;
                for e in children(node, is_expr) {
                    let id = self.expr(&e);
                    if dots.is_some_and(|d| e.text_range().end() <= d) {
                        start = Some(id);
                    } else {
                        end = Some(id);
                    }
                }
                Expr::Range { start, end }
            }
            IS_EXPR => Expr::Is {
                expr: self.expr_opt(child(node, is_expr)),
                pat: self.pat_opt(child(node, is_pat)),
            },
            AS_EXPR => Expr::As {
                expr: self.expr_opt(child(node, is_expr)),
                ty: self.type_opt(child(node, is_type)),
            },
            SWITCH_EXPR => Expr::Switch {
                scrutinee: self.expr_opt(child(node, is_expr)),
                arms: node
                    .children()
                    .filter(|c| c.kind() == SWITCH_ARM)
                    .map(|arm| SwitchArm {
                        pat: self.pat_opt(child(&arm, is_pat)),
                        guard: child_of_kind(&arm, WHEN_CLAUSE)
                            .map(|w| self.expr_opt(child(&w, is_expr))),
                        expr: self.expr_opt(child(&arm, is_expr)),
                    })
                    .collect(),
            },
            WITH_EXPR => Expr::With {
                expr: self.expr_opt(child(node, is_expr)),
                inits: child_of_kind(node, INITIALIZER)
                    .map(|i| self.inits(&i))
                    .unwrap_or_default(),
            },
            OBJECT_INIT_EXPR => Expr::ObjectInit {
                ty: child(node, is_expr).map(|t| self.expr_as_type(&t)),
                inits: child_of_kind(node, INITIALIZER)
                    .map(|i| self.inits(&i))
                    .unwrap_or_default(),
            },
            DEFAULT_EXPR => Expr::Default(child(node, is_type).map(|t| self.ty(&t))),
            THIS_EXPR => Expr::This,
            BASE_EXPR => Expr::Base,
            NEW_EXPR => Expr::New {
                ty: child(node, is_type).map(|t| self.ty(&t)),
                args: child_of_kind(node, ARG_LIST).map(|l| self.args(&l)),
                inits: child_of_kind(node, INITIALIZER).map(|i| self.inits(&i)),
            },
            TYPEOF_EXPR => Expr::Typeof(self.type_opt(child(node, is_type))),
            SIZEOF_EXPR => Expr::Sizeof(self.type_opt(child(node, is_type))),
            THROW_EXPR => Expr::Throw(child(node, is_expr).map(|e| self.expr(&e))),
            COLLECTION_EXPR => Expr::Collection(exprs(self)),
            TUPLE_EXPR => Expr::Tuple(
                node.children()
                    .filter(|c| c.kind() == ARG)
                    .map(|a| {
                        let name = child_of_kind(&a, NAME_COLON).and_then(|n| name_of(&n));
                        (name, self.expr_opt(child(&a, is_expr)))
                    })
                    .collect(),
            ),
            LAMBDA_EXPR => {
                let params = child_of_kind(node, LAMBDA_PARAM_LIST)
                    .map(|l| self.local_params(&l, BindingKind::LambdaParam))
                    .unwrap_or_default();
                let body = match child_of_kind(node, BLOCK) {
                    Some(block) => LambdaBody::Block(self.stmt(&block)),
                    None => LambdaBody::Expr(self.expr_opt(child(node, is_expr))),
                };
                Expr::Lambda { params, body }
            }
            DECL_EXPR => self.decl_expr(node),
            _ => Expr::Missing,
        };
        self.alloc_expr(expr, Some(node))
    }

    /// `var x`, `float3 n`, `var (a, b)`
    fn decl_expr(&mut self, node: &SyntaxNode) -> Expr {
        if let Some(designation) = child_of_kind(node, TUPLE_DESIGNATION) {
            let bindings = designation
                .children()
                .filter(|c| c.kind() == NAME)
                .filter_map(|n| {
                    let name = Name::new(n.first_token()?.text());
                    Some(self.binding(name, BindingKind::Declared, Some(&n)))
                })
                .collect();
            return Expr::DeclTuple(bindings);
        }
        let ty = child(node, is_type)
            .filter(|t| !is_var(t))
            .map(|t| self.ty(&t));
        match self.named_binding(node, BindingKind::Declared) {
            Some(binding) => Expr::DeclVar { ty, binding },
            None => Expr::Missing,
        }
    }

    pub(crate) fn args(&mut self, list: &SyntaxNode) -> Vec<Arg> {
        list.children()
            .filter(|c| c.kind() == ARG)
            .map(|a| Arg {
                name: child_of_kind(&a, NAME_COLON).and_then(|n| name_of(&n)),
                modifier: ref_kind(&a),
                expr: self.expr_opt(child(&a, is_expr)),
            })
            .collect()
    }

    fn inits(&mut self, initializer: &SyntaxNode) -> Vec<FieldInit> {
        initializer
            .children()
            .filter(|c| c.kind() == FIELD_INIT)
            .filter_map(|f| {
                Some(FieldInit {
                    name: name_of(&f)?,
                    value: self.expr_opt(child(&f, is_expr)),
                })
            })
            .collect()
    }

    /// Parameters of a lambda or local function.
    pub(crate) fn local_params(&mut self, list: &SyntaxNode, kind: BindingKind) -> Vec<LocalParam> {
        list.children()
            .filter(|c| c.kind() == PARAM)
            .filter_map(|p| {
                let binding = self.named_binding(&p, kind)?;
                Some(LocalParam {
                    binding,
                    modifier: ref_kind(&p),
                    ty: child(&p, is_type).map(|t| self.ty(&t)),
                })
            })
            .collect()
    }

    // ---- patterns ----

    fn pat_opt(&mut self, node: Option<SyntaxNode>) -> PatId {
        match node {
            Some(node) => self.pat(&node),
            None => self.alloc_pat(Pat::Missing, None),
        }
    }

    fn pat(&mut self, node: &SyntaxNode) -> PatId {
        if node.kind() == PAREN_PATTERN {
            return self.pat_opt(child(node, is_pat));
        }
        let pat = match node.kind() {
            CONSTANT_PATTERN => Pat::Constant(self.expr_opt(child(node, is_expr))),
            RELATIONAL_PATTERN => {
                let op = token(node, |k| !k.is_trivia())
                    .and_then(|t| binary_op(t.kind()))
                    .unwrap_or(BinaryOp::Eq);
                Pat::Relational {
                    op,
                    value: self.expr_opt(child(node, is_expr)),
                }
            }
            DISCARD_PATTERN => Pat::Discard,
            VAR_PATTERN => match self.named_binding(node, BindingKind::Declared) {
                Some(binding) => Pat::Var(binding),
                None => Pat::Missing,
            },
            BINARY_PATTERN => {
                let parts = children(node, is_pat);
                let lhs = self.pat_opt(parts.first().cloned());
                let rhs = self.pat_opt(parts.get(1).cloned());
                if token(node, |k| k == AND_KW).is_some() {
                    Pat::And(lhs, rhs)
                } else {
                    Pat::Or(lhs, rhs)
                }
            }
            NOT_PATTERN => Pat::Not(self.pat_opt(child(node, is_pat))),
            PROPERTY_PATTERN => Pat::Property(
                node.children()
                    .filter(|c| c.kind() == SUBPATTERN)
                    .map(|s| {
                        let member = child(&s, is_expr).map(|e| {
                            e.descendants()
                                .filter(|n| n.kind() == NAME_REF)
                                .filter_map(|n| name_of_ref(&n))
                                .collect()
                        });
                        (member, self.pat_opt(child(&s, is_pat)))
                    })
                    .collect(),
            ),
            POSITIONAL_PATTERN => Pat::Positional(
                node.children()
                    .filter(|c| c.kind() == SUBPATTERN)
                    .map(|s| self.pat_opt(child(&s, is_pat)))
                    .collect(),
            ),
            _ => Pat::Missing,
        };
        self.alloc_pat(pat, Some(node))
    }

    // ---- statements ----

    pub(crate) fn stmt(&mut self, node: &SyntaxNode) -> StmtId {
        let stmt = match node.kind() {
            BLOCK => Stmt::Block(
                children(node, is_stmt)
                    .iter()
                    .map(|s| self.stmt(s))
                    .collect(),
            ),
            EMPTY_STMT => Stmt::Empty,
            EXPR_STMT => Stmt::Expr(self.expr_opt(child(node, is_expr))),
            LOCAL_DECL_STMT => self.local_decl(node),
            IF_STMT => {
                let is_const = token(node, |k| k == CONST_KW).is_some();
                let cond = self.expr_opt(child(node, is_expr));
                let then = match child(node, is_stmt) {
                    Some(s) => self.stmt(&s),
                    None => self.alloc_stmt(Stmt::Missing, node),
                };
                let otherwise = child_of_kind(node, ELSE_CLAUSE)
                    .and_then(|e| child(&e, is_stmt))
                    .map(|s| self.stmt(&s));
                Stmt::If {
                    is_const,
                    cond,
                    then,
                    otherwise,
                }
            }
            FOR_STMT => {
                let init = match child_of_kind(node, FOR_INIT) {
                    Some(init) => match child_of_kind(&init, LOCAL_DECL_STMT) {
                        Some(decl) => vec![self.stmt(&decl)],
                        None => children(&init, is_expr)
                            .iter()
                            .map(|e| {
                                let expr = self.expr(e);
                                self.alloc_stmt(Stmt::Expr(expr), e)
                            })
                            .collect(),
                    },
                    None => Vec::new(),
                };
                let cond = child(node, is_expr).map(|e| self.expr(&e));
                let update = child_of_kind(node, FOR_UPDATE)
                    .map(|u| children(&u, is_expr).iter().map(|e| self.expr(e)).collect())
                    .unwrap_or_default();
                let body = self.stmt_opt(node, child(node, is_stmt));
                Stmt::For {
                    init,
                    cond,
                    update,
                    body,
                }
            }
            FOREACH_STMT => {
                let variable = match child_of_kind(node, DECL_EXPR) {
                    Some(decl) => self.expr(&decl),
                    None => {
                        let ty = child(node, is_type)
                            .filter(|t| !is_var(t))
                            .map(|t| self.ty(&t));
                        let expr = match self.named_binding(node, BindingKind::Declared) {
                            Some(binding) => Expr::DeclVar { ty, binding },
                            None => Expr::Missing,
                        };
                        self.alloc_expr(expr, None)
                    }
                };
                let iterable = self.expr_opt(
                    node.children()
                        .filter(|c| is_expr(c.kind()) && c.kind() != DECL_EXPR)
                        .last(),
                );
                let body = self.stmt_opt(node, child(node, is_stmt));
                Stmt::Foreach {
                    variable,
                    iterable,
                    body,
                }
            }
            WHILE_STMT => Stmt::While {
                cond: self.expr_opt(child(node, is_expr)),
                body: self.stmt_opt(node, child(node, is_stmt)),
            },
            DO_STMT => Stmt::Do {
                body: self.stmt_opt(node, child(node, is_stmt)),
                cond: self.expr_opt(child(node, is_expr)),
            },
            SWITCH_STMT => Stmt::Switch {
                scrutinee: self.expr_opt(child(node, is_expr)),
                sections: node
                    .children()
                    .filter(|c| c.kind() == SWITCH_SECTION)
                    .map(|s| SwitchSection {
                        labels: s
                            .children()
                            .filter_map(|l| match l.kind() {
                                CASE_LABEL => Some(Some((
                                    self.pat_opt(child(&l, is_pat)),
                                    child_of_kind(&l, WHEN_CLAUSE)
                                        .map(|w| self.expr_opt(child(&w, is_expr))),
                                ))),
                                DEFAULT_LABEL => Some(None),
                                _ => None,
                            })
                            .collect(),
                        body: children(&s, is_stmt)
                            .iter()
                            .map(|st| self.stmt(st))
                            .collect(),
                    })
                    .collect(),
            },
            BREAK_STMT => Stmt::Break,
            CONTINUE_STMT => Stmt::Continue,
            RETURN_STMT => Stmt::Return(child(node, is_expr).map(|e| self.expr(&e))),
            FUNCTION_DECL => self.local_function(node),
            _ => Stmt::Missing,
        };
        self.alloc_stmt(stmt, node)
    }

    fn stmt_opt(&mut self, parent: &SyntaxNode, node: Option<SyntaxNode>) -> StmtId {
        match node {
            Some(node) => self.stmt(&node),
            None => self.alloc_stmt(Stmt::Missing, parent),
        }
    }

    fn local_decl(&mut self, node: &SyntaxNode) -> Stmt {
        let attributes = self.attributes(node);
        let is_const = child_of_kind(node, MODIFIERS)
            .is_some_and(|m| m.child_tokens().any(|t| t.kind() == CONST_KW));
        let ty = child(node, is_type)
            .filter(|t| !is_var(t))
            .map(|t| self.ty(&t));
        let declarators = node
            .children()
            .filter(|c| c.kind() == VARIABLE_DECLARATOR)
            .filter_map(|d| {
                let binding = self.named_binding(&d, BindingKind::Local { is_const })?;
                let init = child(&d, is_expr).map(|e| self.expr(&e));
                Some((binding, init))
            })
            .collect();
        Stmt::Local {
            attributes,
            is_const,
            ty,
            declarators,
        }
    }

    fn local_function(&mut self, node: &SyntaxNode) -> Stmt {
        let Some(binding) = self.named_binding(node, BindingKind::LocalFunction) else {
            return Stmt::Missing;
        };
        let name = self.store.bindings[binding].name.clone();
        let is_static = child_of_kind(node, MODIFIERS)
            .is_some_and(|m| m.child_tokens().any(|t| t.kind() == STATIC_KW));
        let generic_params = child_of_kind(node, TYPE_PARAM_LIST)
            .map(|l| {
                l.children()
                    .filter(|p| p.kind() == TYPE_PARAM)
                    .filter_map(|p| name_of(&p))
                    .collect()
            })
            .unwrap_or_default();
        let ret = self.type_opt(child(node, is_type));
        let params = child_of_kind(node, PARAM_LIST)
            .map(|l| self.local_params(&l, BindingKind::LocalFunctionParam))
            .unwrap_or_default();
        let body = self.function_body(node);
        Stmt::LocalFunction(Box::new(LocalFunction {
            name,
            binding,
            is_static,
            generic_params,
            params,
            ret,
            body,
        }))
    }

    /// The `{ ... }` or `=> expr` of a declaration node.
    pub(crate) fn function_body(&mut self, node: &SyntaxNode) -> Option<LambdaBody> {
        if let Some(block) = child_of_kind(node, BLOCK) {
            return Some(LambdaBody::Block(self.stmt(&block)));
        }
        let body = child_of_kind(node, EXPR_BODY)?;
        Some(LambdaBody::Expr(self.expr_opt(child(&body, is_expr))))
    }
}

fn name_of_ref(node: &SyntaxNode) -> Option<Name> {
    node.first_token().map(|t| Name::new(t.text()))
}
