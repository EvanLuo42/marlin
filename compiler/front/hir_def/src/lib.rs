//! Packages, namespaces, declarations and name resolution for the C#-family languages
//! (`docs/design/0015-language-frontend.md` §3).
//!
//! The host sets three kinds of input: the [`Language`] being compiled, every package
//! ([`PackageInput`]: name, files, direct dependencies) and every file's text ([`FileText`]).
//! From them:
//! - [`ParseFile`] parses a file with the language's dialect;
//! - [`AstIdMap`] numbers the declarations of a file, and [`ItemTreeQuery`] summarizes them
//!   without bodies or positions, so edits inside a body leave it equal;
//! - [`PackageDefMap`] merges a package's files into namespaces, and [`FileImports`] resolves a
//!   file's `using` directives;
//! - [`Resolver`] looks names up with C#'s rules.
//!
//! Diagnostics found here are [`DefDiagnostic`]s: they point at an [`AstId`], not a text range,
//! so the values that carry them stay equal across edits that only move text.

mod ast_id;
mod def_map;
mod diagnostics;
mod imports;
mod item_tree;
mod resolver;

use std::fmt;
use std::sync::Arc;

use marlin_front_syntax::{Dialect, Parse};
use marlin_lang_query::{Cancelled, Cx, Input, Query};

pub use ast_id::{AstId, AstIdMap, AstPtr};
pub use def_map::{DefMap, NamespaceScope, PackageDefMap, VisiblePackages};
pub use diagnostics::{DefDiagnostic, DefDiagnosticKind, FileDiagnostics};
pub use imports::{AliasTarget, FileImports, Imports, PackageImports};
pub use item_tree::{
    GenericParam, Item, ItemKind, ItemTree, ItemTreeQuery, LocalItemId, Path, PathSegment, Using,
    UsingKind, Visibility,
};
pub use marlin_lang_span::FileId;
pub use resolver::{Resolution, Resolver, item_path};

/// An identifier as written, without `@`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(Arc<str>);

impl Name {
    pub fn new(text: &str) -> Self {
        Self(text.strip_prefix('@').unwrap_or(text).into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A package, numbered by the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageId(pub u32);

/// What the shared front end needs to know about the language being compiled.
#[derive(Debug)]
pub struct Language {
    pub name: &'static str,
    pub dialect: &'static Dialect,
    /// Namespaces every file imports without a `using` (the shader language's `Marlin`).
    pub implicit_usings: &'static [&'static str],
    /// Whether every file must declare a file-scoped `namespace`.
    pub require_namespace: bool,
}

impl PartialEq for Language {
    /// Languages are `static` descriptions: equal when they are the same one.
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for Language {}

/// The language of this database.
pub struct LanguageInput;
impl Input for LanguageInput {
    type Key = ();
    type Value = &'static Language;
}

/// A package as the host read it from its manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageData {
    pub name: Arc<str>,
    /// In a stable order, the host's choice (usually sorted by path).
    pub files: Arc<[FileId]>,
    /// Direct dependencies.
    pub dependencies: Arc<[PackageId]>,
}

pub struct PackageInput;
impl Input for PackageInput {
    type Key = PackageId;
    type Value = Arc<PackageData>;
}

/// The package each file belongs to.
pub struct FilePackage;
impl Input for FilePackage {
    type Key = FileId;
    type Value = PackageId;
}

pub struct FileText;
impl Input for FileText {
    type Key = FileId;
    type Value = Arc<str>;
}

/// A file parsed with the language's dialect.
pub struct ParseFile;
impl Query for ParseFile {
    type Key = FileId;
    type Value = Arc<Parse>;
    fn execute(cx: &Cx<'_>, file: &FileId) -> Result<Arc<Parse>, Cancelled> {
        let text = cx.input::<FileText>(file)?;
        let language = cx.input::<LanguageInput>(&())?;
        Ok(Arc::new(marlin_front_syntax::parse(
            &text,
            language.dialect,
        )))
    }
}

/// A declaration: an item of some file's item tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemId {
    pub file: FileId,
    pub local: LocalItemId,
}

/// A namespace by its full name; the root namespace is empty. Namespaces are shared by all
/// packages.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NamespacePath(Arc<[Name]>);

impl NamespacePath {
    pub fn root() -> Self {
        Self::default()
    }

    pub fn new(names: impl IntoIterator<Item = Name>) -> Self {
        Self(names.into_iter().collect())
    }

    pub fn names(&self) -> &[Name] {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub fn parent(&self) -> Option<NamespacePath> {
        let (_, init) = self.0.split_last()?;
        Some(Self(init.into()))
    }

    pub fn child(&self, name: &Name) -> NamespacePath {
        Self(self.0.iter().cloned().chain([name.clone()]).collect())
    }

    /// This namespace and its ancestors, innermost first, ending with the root.
    pub fn ancestors(&self) -> impl Iterator<Item = NamespacePath> {
        std::iter::successors(Some(self.clone()), NamespacePath::parent)
    }
}

impl fmt::Display for NamespacePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("<root>");
        }
        for (i, name) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            f.write_str(name.as_str())?;
        }
        Ok(())
    }
}

impl fmt::Debug for NamespacePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NamespacePath({self})")
    }
}
