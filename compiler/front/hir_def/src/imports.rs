//! `using` directives, resolved.

use std::collections::BTreeMap;
use std::sync::Arc;

use marlin_lang_query::{Cancelled, Cx, Query};
use marlin_lang_span::FileId;

use crate::diagnostics::{DefDiagnostic, DefDiagnosticKind};
use crate::item_tree::{Path, Using, UsingKind};
use crate::resolver::{Target, resolve_absolute};
use crate::{
    FilePackage, ItemId, ItemTreeQuery, LanguageInput, Name, NamespacePath, PackageId, PackageInput,
};

/// The usings that apply at one level of name lookup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Imports {
    /// `using A.B;`
    pub namespaces: Vec<NamespacePath>,
    /// `using static A.B.T;`
    pub statics: Vec<ItemId>,
    /// `using X = ...;`
    pub aliases: BTreeMap<Name, AliasTarget>,
    pub diagnostics: Vec<DefDiagnostic>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AliasTarget {
    Namespace(NamespacePath),
    Type(ItemId),
}

impl Imports {
    fn add(
        &mut self,
        cx: &Cx<'_>,
        package: PackageId,
        start: &NamespacePath,
        file: FileId,
        using: &Using,
    ) -> Result<(), Cancelled> {
        let Some(path) = &using.path else {
            return Ok(());
        };
        let target = resolve_absolute(cx, package, start, path)?;
        let expected = match (&using.kind, target) {
            (UsingKind::Namespace, Some(Target::Namespace(ns))) => {
                self.namespaces.push(ns);
                return Ok(());
            }
            (UsingKind::Static, Some(Target::Type(item))) => {
                self.statics.push(item);
                return Ok(());
            }
            (UsingKind::Alias(name), Some(target)) => {
                let target = match target {
                    Target::Namespace(ns) => AliasTarget::Namespace(ns),
                    Target::Type(item) => AliasTarget::Type(item),
                };
                self.aliases.insert(name.clone(), target);
                return Ok(());
            }
            (UsingKind::Namespace, _) => "a namespace",
            (UsingKind::Static, _) => "a type",
            (UsingKind::Alias(_), None) => "a namespace or a type",
        };
        self.diagnostics.push(DefDiagnostic {
            file,
            ast: Some(using.ast),
            kind: DefDiagnosticKind::UnresolvedUsing {
                path: path.to_string(),
                expected,
            },
        });
        Ok(())
    }
}

/// A file's own (not `global`) usings. They apply at the level of the file's namespace and
/// are resolved from it outward, without the usings themselves (as in C#).
pub struct FileImports;
impl Query for FileImports {
    type Key = FileId;
    type Value = Arc<Imports>;
    fn execute(cx: &Cx<'_>, file: &FileId) -> Result<Arc<Imports>, Cancelled> {
        let package = cx.input::<FilePackage>(file)?;
        let tree = cx.get::<ItemTreeQuery>(file)?;
        let namespace = file_namespace(&tree);
        let mut imports = Imports::default();
        for using in tree.usings.iter().filter(|u| !u.global) {
            imports.add(cx, package, &namespace, *file, using)?;
        }
        Ok(Arc::new(imports))
    }
}

/// The `global using`s of all files of a package plus the language's implicit usings. They
/// apply at the root level of every file of the package.
pub struct PackageImports;
impl Query for PackageImports {
    type Key = PackageId;
    type Value = Arc<Imports>;
    fn execute(cx: &Cx<'_>, package: &PackageId) -> Result<Arc<Imports>, Cancelled> {
        let root = NamespacePath::root();
        let mut imports = Imports::default();
        let language = cx.input::<LanguageInput>(&())?;
        for implicit in language.implicit_usings {
            // A missing core library shows up as unresolved names where they are used.
            if let Some(Target::Namespace(ns)) =
                resolve_absolute(cx, *package, &root, &Path::parse(implicit))?
            {
                imports.namespaces.push(ns);
            }
        }
        for &file in cx.input::<PackageInput>(package)?.files.iter() {
            let tree = cx.get::<ItemTreeQuery>(&file)?;
            for using in tree.usings.iter().filter(|u| u.global) {
                imports.add(cx, *package, &root, file, using)?;
            }
        }
        Ok(Arc::new(imports))
    }
}

pub(crate) fn file_namespace(tree: &crate::ItemTree) -> NamespacePath {
    tree.namespace()
        .map(|p| NamespacePath::new(p.segments.iter().map(|s| s.name.clone())))
        .unwrap_or_default()
}
