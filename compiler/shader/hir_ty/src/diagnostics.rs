//! Type-level diagnostics. Like `hir_def`'s, they point at HIR ids (of an item's signature
//! store) instead of text ranges, so the values carrying them survive edits that move text.

use std::sync::Arc;

use marlin_front_hir_def::store::{ExprId, TypeRefId};
use marlin_front_hir_def::{
    AstIdMap, FileDiagnostics, FileId, FilePackage, ItemId, ItemTreeQuery, ParseFile,
    SignatureWithSourceMap,
};
use marlin_front_syntax::SyntaxKind;
use marlin_lang_diag::{Code, Diagnostic, Span};
use marlin_lang_query::{Cancelled, Cx, Query};

use crate::builtins::BuiltinsQuery;
use crate::item::ItemTypesQuery;

/// Where in an item's signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TyLoc {
    /// The item's name.
    Item,
    Type(TypeRefId),
    Expr(ExprId),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TyDiagnostic {
    pub item: ItemId,
    pub loc: TyLoc,
    pub kind: TyDiagnosticKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TyDiagnosticKind {
    UnresolvedType {
        path: String,
    },
    NotAType {
        path: String,
        what: &'static str,
    },
    WrongArity {
        path: String,
        expected: usize,
        found: usize,
    },
    /// The argument for constant parameter `param` is a type, or the other way round.
    ArgumentKind {
        param: String,
        expected_const: bool,
    },
    InaccessibleType {
        path: String,
    },
    AmbiguousType {
        path: String,
    },
    UnknownBuiltin {
        name: String,
    },
    /// The core library declares no `[Builtin(name)]` type, which `keyword` needs.
    MissingBuiltin {
        keyword: &'static str,
        name: &'static str,
    },
    /// A C# keyword type the language does not have.
    UnsupportedType {
        keyword: &'static str,
    },
    AliasCycle,
    BadArrayLength,
    NotConstant,
    NotAnInterface {
        ty: String,
    },
    BadConstParamType {
        ty: String,
    },
    /// `[Builtin]` or `[Intrinsic]` outside the core library.
    AttributeOutsideCore {
        attribute: &'static str,
    },
    /// A function without a body that is neither `[Intrinsic]` nor in an interface.
    MissingBody,
}

impl TyDiagnostic {
    pub fn code(&self) -> Code {
        use TyDiagnosticKind::*;
        Code(match self.kind {
            UnresolvedType { .. } => "E1001",
            NotAType { .. } => "E1002",
            WrongArity { .. } => "E1003",
            ArgumentKind { .. } => "E1004",
            UnknownBuiltin { .. } | MissingBuiltin { .. } | AttributeOutsideCore { .. } => "E1005",
            UnsupportedType { .. } => "E1006",
            AliasCycle => "E1007",
            BadArrayLength | NotConstant => "E1008",
            InaccessibleType { .. } => "E1009",
            AmbiguousType { .. } => "E1010",
            NotAnInterface { .. } => "E1011",
            BadConstParamType { .. } => "E1012",
            MissingBody => "E1013",
        })
    }

    pub fn message(&self) -> String {
        use TyDiagnosticKind::*;
        match &self.kind {
            UnresolvedType { path } => format!("cannot find type `{path}`"),
            NotAType { path, what } => format!("`{path}` is {what}, not a type"),
            WrongArity {
                path,
                expected,
                found,
            } => format!("`{path}` takes {expected} type argument(s), {found} given"),
            ArgumentKind {
                param,
                expected_const: true,
            } => format!("`{param}` is a constant parameter; it takes a constant, not a type"),
            ArgumentKind { param, .. } => format!("`{param}` takes a type, not a constant"),
            InaccessibleType { path } => format!("type `{path}` is not accessible here"),
            AmbiguousType { path } => format!("`{path}` is ambiguous between several types"),
            UnknownBuiltin { name } => format!("`{name}` is not a built-in the compiler knows"),
            MissingBuiltin { keyword, name } => format!(
                "the core library declares no `[Builtin(\"{name}\")]` type, which `{keyword}` needs"
            ),
            UnsupportedType { keyword } => format!("Marlin shader has no `{keyword}` type"),
            AliasCycle => "this type alias refers to itself".into(),
            BadArrayLength => "an array length must be a positive constant".into(),
            NotConstant => "this must be a compile-time constant".into(),
            NotAnInterface { ty } => {
                format!("`{ty}` is not an interface; constraints name interfaces")
            }
            BadConstParamType { ty } => {
                format!("constant parameters are `int` or `uint`, not `{ty}`")
            }
            AttributeOutsideCore { attribute } => {
                format!("`[{attribute}]` is only for the core library")
            }
            MissingBody => {
                "a function without a body must be `[Intrinsic]` or declared in an interface".into()
            }
        }
    }

    pub fn to_diagnostic(&self, cx: &Cx<'_>) -> Result<Diagnostic, Cancelled> {
        let file = self.item.file;
        let range = match self.loc {
            TyLoc::Item => {
                let tree = cx.get::<ItemTreeQuery>(&file)?;
                let ids = cx.get::<AstIdMap>(&file)?;
                let parse = cx.get::<ParseFile>(&file)?;
                let node = ids
                    .ptr(tree.items[self.item.local].ast)
                    .to_node(&parse.syntax_node());
                let name = node.children().find(|n| n.kind() == SyntaxKind::NAME);
                name.unwrap_or(node).text_range()
            }
            TyLoc::Type(t) => {
                let (_, map) = cx.get::<SignatureWithSourceMap>(&self.item)?;
                match map.types.get(t) {
                    Some(ptr) => ptr.range,
                    None => {
                        return Self {
                            loc: TyLoc::Item,
                            ..self.clone()
                        }
                        .to_diagnostic(cx);
                    }
                }
            }
            TyLoc::Expr(e) => {
                let (_, map) = cx.get::<SignatureWithSourceMap>(&self.item)?;
                match map.exprs.get(e) {
                    Some(ptr) => ptr.range,
                    None => {
                        return Self {
                            loc: TyLoc::Item,
                            ..self.clone()
                        }
                        .to_diagnostic(cx);
                    }
                }
            }
        };
        Ok(Diagnostic::error(self.code(), self.message()).at(Span::new(file, range), ""))
    }
}

/// Every diagnostic of one file so far: syntax, declarations and types.
pub struct FileTyDiagnostics;
impl Query for FileTyDiagnostics {
    type Key = FileId;
    type Value = Arc<[Diagnostic]>;
    fn execute(cx: &Cx<'_>, file: &FileId) -> Result<Arc<[Diagnostic]>, Cancelled> {
        let mut out: Vec<Diagnostic> = cx.get::<FileDiagnostics>(file)?.to_vec();
        let package = cx.input::<FilePackage>(file)?;
        let builtins = cx.get::<BuiltinsQuery>(&package)?;
        for d in builtins.diagnostics.iter().filter(|d| d.item.file == *file) {
            out.push(d.to_diagnostic(cx)?);
        }
        let tree = cx.get::<ItemTreeQuery>(file)?;
        for (local, _) in tree.items.iter() {
            let item = ItemId { file: *file, local };
            for d in &cx.get::<ItemTypesQuery>(&item)?.diagnostics {
                out.push(d.to_diagnostic(cx)?);
            }
        }
        out.sort_by_key(|d| d.primary.as_ref().map(|l| l.span.range.start()));
        Ok(out.into())
    }
}
