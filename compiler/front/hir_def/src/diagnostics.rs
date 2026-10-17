//! Diagnostics of the declaration level. They point at declarations by [`AstId`], so they do
//! not change when text moves; [`DefDiagnostic::to_diagnostic`] finds the current range.

use std::sync::Arc;

use marlin_front_syntax::SyntaxKind;
use marlin_lang_diag::{Code, Diagnostic, Span};
use marlin_lang_query::{Cancelled, Cx, Query};
use marlin_lang_span::{FileId, TextRange, TextSize};

use crate::ast_id::{AstId, AstIdMap};
use crate::{FileImports, FilePackage, PackageDefMap, ParseFile};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DefDiagnostic {
    pub file: FileId,
    /// The declaration it is about; `None` for the file as a whole.
    pub ast: Option<AstId>,
    pub kind: DefDiagnosticKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DefDiagnosticKind {
    /// The language requires a file-scoped `namespace` and the file has none.
    MissingNamespace,
    /// A second file-scoped `namespace` declaration.
    ExtraNamespace,
    /// `what` named `name` is already declared at (`first_file`, `first`).
    Duplicate {
        name: String,
        what: &'static str,
        first_file: FileId,
        first: AstId,
    },
    /// A kind of declaration that only exists inside a type.
    NotInNamespace { what: &'static str },
    /// A `using` path that names nothing (or not what that kind of `using` needs).
    UnresolvedUsing {
        path: String,
        expected: &'static str,
    },
}

impl DefDiagnostic {
    pub fn code(&self) -> Code {
        match self.kind {
            DefDiagnosticKind::Duplicate { .. } => Code("E0401"),
            DefDiagnosticKind::MissingNamespace => Code("E0402"),
            DefDiagnosticKind::ExtraNamespace => Code("E0402"),
            DefDiagnosticKind::UnresolvedUsing { .. } => Code("E0403"),
            DefDiagnosticKind::NotInNamespace { .. } => Code("E0404"),
        }
    }

    /// The diagnostic with ranges in the file's current text.
    pub fn to_diagnostic(&self, cx: &Cx<'_>) -> Result<Diagnostic, Cancelled> {
        let at = name_span(cx, self.file, self.ast)?;
        let d = match &self.kind {
            DefDiagnosticKind::MissingNamespace => {
                Diagnostic::error(self.code(), "this file has no namespace declaration")
                    .at(at, "")
                    .note("start the file with a file-scoped namespace: `namespace A.B;`")
            }
            DefDiagnosticKind::ExtraNamespace => Diagnostic::error(
                self.code(),
                "a file has only one file-scoped namespace declaration",
            )
            .at(at, "second declaration"),
            DefDiagnosticKind::Duplicate {
                name,
                what,
                first_file,
                first,
            } => Diagnostic::error(self.code(), format!("{what} `{name}` is defined twice"))
                .at(at, "redefined here")
                .label(
                    name_span(cx, *first_file, Some(*first))?,
                    "first defined here",
                ),
            DefDiagnosticKind::NotInNamespace { what } => Diagnostic::error(
                self.code(),
                format!("a {what} can only be declared inside a type"),
            )
            .at(at, ""),
            DefDiagnosticKind::UnresolvedUsing { path, expected } => {
                Diagnostic::error(self.code(), format!("`{path}` is not {expected}")).at(at, "")
            }
        };
        Ok(d)
    }
}

/// The name of declaration `ast` (or the whole declaration if it has none); the start of the
/// file for `None`.
fn name_span(cx: &Cx<'_>, file: FileId, ast: Option<AstId>) -> Result<Span, Cancelled> {
    let Some(ast) = ast else {
        return Ok(Span::new(file, TextRange::empty(TextSize(0))));
    };
    let ids = cx.get::<AstIdMap>(&file)?;
    let parse = cx.get::<ParseFile>(&file)?;
    let node = ids.ptr(ast).to_node(&parse.syntax_node());
    let named = node.descendants().find(|n| {
        matches!(
            n.kind(),
            SyntaxKind::NAME | SyntaxKind::PATH | SyntaxKind::USING_ALIAS
        )
    });
    let named = named.unwrap_or(node);
    Ok(Span::new(file, named.text_range()))
}

/// Every syntax and declaration diagnostic of one file, in source order.
pub struct FileDiagnostics;
impl Query for FileDiagnostics {
    type Key = FileId;
    type Value = Arc<[Diagnostic]>;
    fn execute(cx: &Cx<'_>, file: &FileId) -> Result<Arc<[Diagnostic]>, Cancelled> {
        let mut out = cx.get::<ParseFile>(file)?.diagnostics(*file);
        let package = cx.input::<FilePackage>(file)?;
        let def_map = cx.get::<PackageDefMap>(&package)?;
        for d in def_map.diagnostics.iter().filter(|d| d.file == *file) {
            out.push(d.to_diagnostic(cx)?);
        }
        for d in &cx.get::<FileImports>(file)?.diagnostics {
            out.push(d.to_diagnostic(cx)?);
        }
        for d in cx
            .get::<crate::PackageImports>(&package)?
            .diagnostics
            .iter()
            .filter(|d| d.file == *file)
        {
            out.push(d.to_diagnostic(cx)?);
        }
        // Locals that reuse a name of an enclosing scope.
        let tree = cx.get::<crate::ItemTreeQuery>(file)?;
        for (local, _) in tree.items.iter() {
            let item = crate::ItemId { file: *file, local };
            for owner in crate::body_owners(cx, item)? {
                let scopes = cx.get::<crate::ExprScopesQuery>(&owner)?;
                if scopes.redeclared.is_empty() {
                    continue;
                }
                let (body, map) = cx.get::<crate::BodyWithSourceMap>(&owner)?;
                for &(binding, previous) in &scopes.redeclared {
                    let Some(ptr) = map.bindings.get(binding) else {
                        continue;
                    };
                    let name = &body.store.bindings[binding].name;
                    let mut d = Diagnostic::error(
                        Code("E0405"),
                        format!("a local named `{name}` conflicts with another local of that name"),
                    )
                    .at(Span::new(*file, ptr.range), "")
                    .note("a local's name must be unique in its block, nested blocks included");
                    if let Some(previous) = map.bindings.get(previous) {
                        d = d.label(Span::new(*file, previous.range), "first declared here");
                    }
                    out.push(d);
                }
            }
        }
        out.sort_by_key(|d| d.primary.as_ref().map(|l| l.span.range.start()));
        Ok(out.into())
    }
}
