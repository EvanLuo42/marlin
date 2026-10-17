//! Type references (as written) → semantic types.

use std::sync::Arc;

use marlin_front_hir_def::store::{
    BinaryOp, Expr, ExprId, ExprStore, GenericArg, HirPath, Literal, Predefined, TypeRef,
    TypeRefId, UnaryOp,
};
use marlin_front_hir_def::{ItemId, ItemKind, ItemTreeQuery, Resolution, Resolver};
use marlin_lang_query::{Cancelled, Cx};

use crate::builtins::{BuiltinKind, Builtins, Scalar};
use crate::diagnostics::{TyDiagnostic, TyDiagnosticKind, TyLoc};
use crate::item::AliasTargetQuery;
use crate::ty::{ConstOp, ConstTy, GenericArgTy, Ty};

pub(crate) struct TyLowerer<'a, 'b> {
    pub(crate) cx: &'a Cx<'b>,
    /// The item whose signature store is being lowered.
    pub(crate) owner: ItemId,
    pub(crate) store: &'a ExprStore,
    pub(crate) resolver: Resolver,
    pub(crate) builtins: Arc<Builtins>,
    pub(crate) diagnostics: Vec<TyDiagnostic>,
    /// Inside an attribute declaration, where `string` is allowed.
    pub(crate) in_attribute: bool,
}

impl TyLowerer<'_, '_> {
    fn report(&mut self, loc: TyLoc, kind: TyDiagnosticKind) {
        self.diagnostics.push(TyDiagnostic {
            item: self.owner,
            loc,
            kind,
        });
    }

    pub(crate) fn ty(&mut self, t: TypeRefId) -> Result<Ty, Cancelled> {
        Ok(match &self.store.types[t] {
            TypeRef::Missing => Ty::Error,
            TypeRef::Predefined(p) => self.predefined(*p, t),
            TypeRef::Path(path) => self.path_ty(path, t)?,
            TypeRef::Array { element, len } => {
                let element = self.ty(*element)?;
                let len = match len {
                    Some(e) => self.const_expr(*e)?,
                    None => ConstTy::Error,
                };
                // A non-constant length was reported by `const_expr`.
                if len.value().is_some_and(|v| v <= 0)
                    || matches!(&self.store.types[t], TypeRef::Array { len: None, .. })
                {
                    self.report(TyLoc::Type(t), TyDiagnosticKind::BadArrayLength);
                }
                Ty::Array {
                    element: Arc::new(element),
                    len,
                }
            }
            TypeRef::Tuple(items) => {
                let mut tys = Vec::new();
                for (item, _) in items {
                    tys.push(self.ty(*item)?);
                }
                Ty::Tuple(tys.into())
            }
        })
    }

    fn predefined(&mut self, p: Predefined, t: TypeRefId) -> Ty {
        let scalar = match p {
            Predefined::Void => return Ty::Void,
            Predefined::Bool => Scalar::Bool,
            Predefined::Short => Scalar::I16,
            Predefined::Ushort => Scalar::U16,
            Predefined::Int => Scalar::I32,
            Predefined::Uint => Scalar::U32,
            Predefined::Long => Scalar::I64,
            Predefined::Ulong => Scalar::U64,
            Predefined::Half => Scalar::F16,
            Predefined::Float => Scalar::F32,
            Predefined::Double => Scalar::F64,
            Predefined::String if self.in_attribute => {
                return match self.builtins.item(BuiltinKind::String) {
                    Some(item) => Ty::adt(item),
                    None => {
                        self.report(
                            TyLoc::Type(t),
                            TyDiagnosticKind::MissingBuiltin {
                                keyword: "string",
                                name: "string",
                            },
                        );
                        Ty::Error
                    }
                };
            }
            Predefined::Byte
            | Predefined::Sbyte
            | Predefined::Char
            | Predefined::Decimal
            | Predefined::Object
            | Predefined::String => {
                self.report(
                    TyLoc::Type(t),
                    TyDiagnosticKind::UnsupportedType {
                        keyword: p.keyword(),
                    },
                );
                return Ty::Error;
            }
        };
        match self.builtins.item(BuiltinKind::Scalar(scalar)) {
            Some(item) => Ty::adt(item),
            None => {
                self.report(
                    TyLoc::Type(t),
                    TyDiagnosticKind::MissingBuiltin {
                        keyword: scalar.keyword(),
                        name: scalar.builtin_name(),
                    },
                );
                Ty::Error
            }
        }
    }

    fn path_ty(&mut self, path: &HirPath, t: TypeRefId) -> Result<Ty, Cancelled> {
        self.path_type(path, TyLoc::Type(t))
    }

    /// The type a path names, reporting problems at `loc`.
    pub(crate) fn path_type(&mut self, path: &HirPath, loc: TyLoc) -> Result<Ty, Cancelled> {
        let def_path = path.to_def_path();
        let text = def_path.to_string();
        let resolution = self.resolver.resolve(self.cx, &def_path, false)?;
        Ok(match resolution {
            Resolution::Type(item) => {
                let args = path.segments.last().and_then(|s| s.args.as_deref());
                self.adt(item, args, loc, &text)?
            }
            Resolution::TypeParam { owner, index } => {
                if self.param_is_const(owner, index)? {
                    self.not_a_type(loc, &text, "a constant parameter")
                } else {
                    Ty::Param { owner, index }
                }
            }
            Resolution::Partial { base, resolved } => match *base {
                Resolution::TypeParam { owner, index } => {
                    let mut ty = Ty::Param { owner, index };
                    for segment in &def_path.segments[resolved..] {
                        ty = Ty::Assoc {
                            base: Arc::new(ty),
                            name: segment.name.clone(),
                        };
                    }
                    ty
                }
                _ => self.not_a_type(loc, &text, "a member"),
            },
            Resolution::Namespace(_) => self.not_a_type(loc, &text, "a namespace"),
            Resolution::Values(_) | Resolution::Local(_) => {
                self.not_a_type(loc, &text, "a function or field")
            }
            Resolution::Unresolved { .. } => {
                // Name what the path is, if it is something other than a type.
                if let Resolution::Values(_) = self.resolver.resolve(self.cx, &def_path, true)? {
                    return Ok(self.not_a_type(loc, &text, "a function or field"));
                }
                self.report(loc, TyDiagnosticKind::UnresolvedType { path: text });
                Ty::Error
            }
            Resolution::Inaccessible { .. } => {
                self.report(loc, TyDiagnosticKind::InaccessibleType { path: text });
                Ty::Error
            }
            Resolution::Ambiguous { .. } => {
                self.report(loc, TyDiagnosticKind::AmbiguousType { path: text });
                Ty::Error
            }
        })
    }

    fn not_a_type(&mut self, loc: TyLoc, path: &str, what: &'static str) -> Ty {
        self.report(
            loc,
            TyDiagnosticKind::NotAType {
                path: path.to_string(),
                what,
            },
        );
        Ty::Error
    }

    fn param_is_const(&self, owner: ItemId, index: u32) -> Result<bool, Cancelled> {
        let tree = self.cx.get::<ItemTreeQuery>(&owner.file)?;
        Ok(tree.items[owner.local]
            .generic_params
            .get(index as usize)
            .is_some_and(|p| p.is_const))
    }

    /// A named type with its arguments, expanding aliases.
    fn adt(
        &mut self,
        item: ItemId,
        args: Option<&[GenericArg]>,
        loc: TyLoc,
        text: &str,
    ) -> Result<Ty, Cancelled> {
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        let data = &tree.items[item.local];
        if matches!(data.kind, ItemKind::TypeAlias | ItemKind::AssocType) {
            if args.is_some_and(|a| !a.is_empty()) {
                self.report(
                    loc,
                    TyDiagnosticKind::WrongArity {
                        path: text.to_string(),
                        expected: 0,
                        found: args.map_or(0, <[_]>::len),
                    },
                );
                return Ok(Ty::Error);
            }
            return Ok(match self.cx.get::<AliasTargetQuery>(&item)? {
                Some(target) => target,
                // An interface's associated type: resolved with its implementer later.
                None => Ty::adt(item),
            });
        }
        let params = data.generic_params.clone();
        let args = args.unwrap_or_default();
        if args.len() != params.len() {
            self.report(
                loc,
                TyDiagnosticKind::WrongArity {
                    path: text.to_string(),
                    expected: params.len(),
                    found: args.len(),
                },
            );
            return Ok(Ty::Error);
        }
        let mut lowered = Vec::with_capacity(args.len());
        for (param, arg) in params.iter().zip(args) {
            lowered.push(match (param.is_const, arg) {
                (false, GenericArg::Type(t)) => GenericArgTy::Type(self.ty(*t)?),
                (true, GenericArg::Const(e)) => GenericArgTy::Const(self.const_expr(*e)?),
                (true, GenericArg::Type(t)) => {
                    GenericArgTy::Const(self.const_from_type(*t, &param.name)?)
                }
                (false, GenericArg::Const(e)) => {
                    self.report(
                        TyLoc::Expr(*e),
                        TyDiagnosticKind::ArgumentKind {
                            param: param.name.to_string(),
                            expected_const: false,
                        },
                    );
                    GenericArgTy::Type(Ty::Error)
                }
            });
        }
        Ok(Ty::Adt {
            item,
            args: lowered.into(),
        })
    }

    /// A constant argument written where the parser saw a type: a lone name such as `N`.
    fn const_from_type(
        &mut self,
        t: TypeRefId,
        param: &marlin_front_hir_def::Name,
    ) -> Result<ConstTy, Cancelled> {
        if let TypeRef::Path(path) = &self.store.types[t]
            && let Some(c) = self.const_path(path)?
        {
            return Ok(c);
        }
        self.report(
            TyLoc::Type(t),
            TyDiagnosticKind::ArgumentKind {
                param: param.to_string(),
                expected_const: true,
            },
        );
        Ok(ConstTy::Error)
    }

    /// A name that denotes a constant: a constant generic parameter or a `const` field.
    fn const_path(&mut self, path: &HirPath) -> Result<Option<ConstTy>, Cancelled> {
        Ok(
            match self.resolver.resolve(self.cx, &path.to_def_path(), true)? {
                Resolution::TypeParam { owner, index } if self.param_is_const(owner, index)? => {
                    Some(ConstTy::Param { owner, index })
                }
                Resolution::Values(items) if items.len() == 1 => {
                    let item = items[0];
                    let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
                    let data = &tree.items[item.local];
                    (data.kind == ItemKind::Field && data.is_const
                        || data.kind == ItemKind::EnumMember)
                        .then_some(ConstTy::Item(item))
                }
                _ => None,
            },
        )
    }

    /// A constant expression: integer literals, constant names, and arithmetic over them.
    pub(crate) fn const_expr(&mut self, e: ExprId) -> Result<ConstTy, Cancelled> {
        let c = match &self.store.exprs[e] {
            Expr::Literal(Literal::Number(text)) => parse_int(text).map(ConstTy::Value),
            Expr::Unary {
                op: UnaryOp::Neg,
                operand,
            } => {
                let operand = self.const_expr(*operand)?;
                Some(ConstTy::apply(ConstOp::Neg, operand, None))
            }
            Expr::Unary {
                op: UnaryOp::Plus,
                operand,
            } => Some(self.const_expr(*operand)?),
            Expr::Binary { op, lhs, rhs } => {
                let op = match op {
                    BinaryOp::Add => Some(ConstOp::Add),
                    BinaryOp::Sub => Some(ConstOp::Sub),
                    BinaryOp::Mul => Some(ConstOp::Mul),
                    BinaryOp::Div => Some(ConstOp::Div),
                    BinaryOp::Rem => Some(ConstOp::Rem),
                    BinaryOp::Shl => Some(ConstOp::Shl),
                    BinaryOp::Shr => Some(ConstOp::Shr),
                    _ => None,
                };
                match op {
                    Some(op) => {
                        let lhs = self.const_expr(*lhs)?;
                        let rhs = self.const_expr(*rhs)?;
                        Some(ConstTy::apply(op, lhs, Some(rhs)))
                    }
                    None => None,
                }
            }
            Expr::Path(path) => self.const_path(path)?,
            _ => None,
        };
        Ok(match c {
            Some(c) => c,
            None => {
                self.report(TyLoc::Expr(e), TyDiagnosticKind::NotConstant);
                ConstTy::Error
            }
        })
    }
}

/// An integer literal's value: decimal, `0x` or `0b`, with `_` separators and a `u`/`l` suffix.
pub(crate) fn parse_int(text: &str) -> Option<i128> {
    let lower = text.to_ascii_lowercase().replace('_', "");
    let (radix, digits) = if let Some(hex) = lower.strip_prefix("0x") {
        (16, hex)
    } else if let Some(bin) = lower.strip_prefix("0b") {
        (2, bin)
    } else {
        (10, lower.as_str())
    };
    let digits = digits.trim_end_matches(['u', 'l']);
    if radix == 10 && digits.contains(['.', 'e', 'f', 'd', 'h', 'm']) {
        return None;
    }
    i128::from_str_radix(digits, radix).ok()
}
