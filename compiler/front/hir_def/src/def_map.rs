//! A package's namespaces: what every namespace declares, merged over the package's files.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use marlin_lang_query::{Cancelled, Cx, Query};

use crate::diagnostics::{DefDiagnostic, DefDiagnosticKind};
use crate::item_tree::{ItemKind, ItemTree, LocalItemId};
use crate::{ItemId, ItemTreeQuery, LanguageInput, Name, NamespacePath, PackageId, PackageInput};

/// The declarations of one namespace within one package.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamespaceScope {
    pub children: BTreeSet<Name>,
    /// Types by name and number of type parameters; more than one entry is a duplicate.
    pub types: BTreeMap<(Name, u32), Vec<ItemId>>,
    /// Functions (an overload set) and fields by name.
    pub values: BTreeMap<Name, Vec<ItemId>>,
    pub extensions: Vec<ItemId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefMap {
    pub package: PackageId,
    /// Every namespace the package declares something in, with all its ancestors.
    pub namespaces: BTreeMap<NamespacePath, NamespaceScope>,
    pub diagnostics: Vec<DefDiagnostic>,
}

impl DefMap {
    pub fn namespace(&self, path: &NamespacePath) -> Option<&NamespaceScope> {
        self.namespaces.get(path)
    }

    /// A readable listing, for tests and debugging.
    pub fn dump(&self, cx: &Cx<'_>) -> Result<String, Cancelled> {
        let mut out = String::new();
        for (path, scope) in &self.namespaces {
            out.push_str(&format!("namespace {path}\n"));
            for child in &scope.children {
                out.push_str(&format!("  namespace {child}\n"));
            }
            for ((name, arity), items) in &scope.types {
                for &item in items {
                    let data = item_data(cx, item)?;
                    let arity = if *arity > 0 {
                        format!("`{arity}")
                    } else {
                        String::new()
                    };
                    out.push_str(&format!(
                        "  {} {name}{arity} ({:?})\n",
                        data.kind.describe(),
                        data.visibility
                    ));
                }
            }
            for (name, items) in &scope.values {
                for &item in items {
                    let data = item_data(cx, item)?;
                    out.push_str(&format!(
                        "  {} {name} ({:?})\n",
                        data.kind.describe(),
                        data.visibility
                    ));
                }
            }
            if !scope.extensions.is_empty() {
                out.push_str(&format!(
                    "  {} extension block(s)\n",
                    scope.extensions.len()
                ));
            }
        }
        Ok(out)
    }
}

pub(crate) fn item_data(cx: &Cx<'_>, item: ItemId) -> Result<crate::Item, Cancelled> {
    Ok(cx.get::<ItemTreeQuery>(&item.file)?.items[item.local].clone())
}

pub struct PackageDefMap;
impl Query for PackageDefMap {
    type Key = PackageId;
    type Value = Arc<DefMap>;
    fn execute(cx: &Cx<'_>, package: &PackageId) -> Result<Arc<DefMap>, Cancelled> {
        let data = cx.input::<PackageInput>(package)?;
        let language = cx.input::<LanguageInput>(&())?;
        let mut collector = Collector {
            map: DefMap {
                package: *package,
                namespaces: BTreeMap::new(),
                diagnostics: Vec::new(),
            },
            trees: HashMap::new(),
        };
        collector.ensure(&NamespacePath::root());
        let mut trees = Vec::new();
        for &file in data.files.iter() {
            let tree = cx.get::<ItemTreeQuery>(&file)?;
            let namespace = match tree.namespace() {
                Some(path) => NamespacePath::new(path.segments.iter().map(|s| s.name.clone())),
                None => {
                    if language.require_namespace {
                        collector.diagnostic(file, None, DefDiagnosticKind::MissingNamespace);
                    }
                    NamespacePath::root()
                }
            };
            for (_, ast) in tree.namespace_decls.iter().skip(1) {
                collector.diagnostic(file, Some(*ast), DefDiagnosticKind::ExtraNamespace);
            }
            collector.collect(&tree, file, &namespace, &tree.top_level);
            collector.trees.insert(file, tree.clone());
            trees.push((file, tree));
        }
        collector.namespace_duplicates(language.type_and_function_names);
        for (file, tree) in &trees {
            collector.member_duplicates(*file, tree);
        }
        Ok(Arc::new(collector.map))
    }
}

struct Collector {
    map: DefMap,
    trees: HashMap<crate::FileId, Arc<ItemTree>>,
}

impl Collector {
    fn diagnostic(
        &mut self,
        file: crate::FileId,
        ast: Option<crate::AstId>,
        kind: DefDiagnosticKind,
    ) {
        self.map.diagnostics.push(DefDiagnostic { file, ast, kind });
    }

    /// Creates `path` and its ancestors, linking each to its parent.
    fn ensure(&mut self, path: &NamespacePath) {
        self.map.namespaces.entry(path.clone()).or_default();
        let mut child = path.clone();
        while let Some(parent) = child.parent() {
            let name = child.names().last().unwrap().clone();
            self.map
                .namespaces
                .entry(parent.clone())
                .or_default()
                .children
                .insert(name);
            child = parent;
        }
    }

    fn collect(
        &mut self,
        tree: &ItemTree,
        file: crate::FileId,
        namespace: &NamespacePath,
        items: &[LocalItemId],
    ) {
        self.ensure(namespace);
        for &local in items {
            let item = &tree.items[local];
            let id = ItemId { file, local };
            match item.kind {
                ItemKind::Namespace => {
                    let mut path = namespace.clone();
                    for segment in item.namespace.iter().flat_map(|p| &p.segments) {
                        path = path.child(&segment.name);
                    }
                    self.collect(tree, file, &path, &item.members);
                }
                kind if kind.is_type() => {
                    if let Some(name) = &item.name {
                        self.scope(namespace)
                            .types
                            .entry((name.clone(), item.arity()))
                            .or_default()
                            .push(id);
                    }
                }
                ItemKind::Function | ItemKind::Field => {
                    if let Some(name) = &item.name {
                        self.scope(namespace)
                            .values
                            .entry(name.clone())
                            .or_default()
                            .push(id);
                    }
                }
                ItemKind::Extension => self.scope(namespace).extensions.push(id),
                kind => self.diagnostic(
                    file,
                    Some(item.ast),
                    DefDiagnosticKind::NotInNamespace {
                        what: kind.describe(),
                    },
                ),
            }
        }
    }

    fn scope(&mut self, namespace: &NamespacePath) -> &mut NamespaceScope {
        self.map.namespaces.get_mut(namespace).expect("ensured")
    }

    /// Types defined twice, fields defined twice or next to functions, and names used by both a
    /// type and a value or namespace (but for names in `shared`: generic types and functions).
    fn namespace_duplicates(&mut self, shared: &[&str]) {
        let mut found = Vec::new();
        for scope in self.map.namespaces.values() {
            for ((name, _), items) in &scope.types {
                for &dup in &items[1..] {
                    found.push((dup, items[0], name.clone(), "type"));
                }
            }
            for (name, items) in &scope.values {
                // An overload set of functions is fine; anything else sharing a name is not.
                let kinds: Vec<_> = items.iter().map(|&i| (i, self.is_function(i))).collect();
                if kinds.iter().any(|&(_, f)| !f) && kinds.len() > 1 {
                    for &(dup, _) in &kinds[1..] {
                        found.push((dup, kinds[0].0, name.clone(), "member"));
                    }
                }
                let type_named = scope
                    .types
                    .iter()
                    .find(|((type_name, _), _)| type_name == name)
                    .map(|(_, items)| items[0]);
                // A generic type of a name the language lets functions share.
                let shared_ok = shared.contains(&name.as_str())
                    && kinds.iter().all(|&(_, f)| f)
                    && scope
                        .types
                        .keys()
                        .all(|(type_name, arity)| type_name != name || *arity > 0);
                if let Some(first) = type_named.filter(|_| !shared_ok) {
                    for &dup in items {
                        found.push((dup, first, name.clone(), "name"));
                    }
                }
            }
        }
        for (dup, first, name, what) in found {
            let ast = self.ast_of(dup);
            let first_ast = self.ast_of(first);
            self.diagnostic(
                dup.file,
                Some(ast),
                DefDiagnosticKind::Duplicate {
                    name: name.to_string(),
                    what,
                    first_file: first.file,
                    first: first_ast,
                },
            );
        }
    }

    /// Members of one type that share a name, other than overloaded methods.
    fn member_duplicates(&mut self, file: crate::FileId, tree: &ItemTree) {
        let mut found = Vec::new();
        for (_, item) in tree.items.iter() {
            if !item.kind.is_type() && item.kind != ItemKind::Extension {
                continue;
            }
            let mut seen: BTreeMap<(&Name, u32), (LocalItemId, ItemKind)> = BTreeMap::new();
            for &member in &item.members {
                let data = &tree.items[member];
                let Some(name) = &data.name else { continue };
                // Constructors carry the type's name; they overload like methods.
                if data.kind == ItemKind::Constructor {
                    continue;
                }
                let arity = if data.kind.is_type() { data.arity() } else { 0 };
                match seen.get(&(name, arity)) {
                    Some(&(_, ItemKind::Function)) if data.kind == ItemKind::Function => {}
                    Some(&(first, _)) => found.push((member, first, name.to_string())),
                    None => {
                        seen.insert((name, arity), (member, data.kind));
                    }
                }
            }
        }
        for (dup, first, name) in found {
            self.diagnostic(
                file,
                Some(tree.items[dup].ast),
                DefDiagnosticKind::Duplicate {
                    name,
                    what: "member",
                    first_file: file,
                    first: tree.items[first].ast,
                },
            );
        }
    }

    fn item(&self, item: ItemId) -> &crate::Item {
        &self.trees[&item.file].items[item.local]
    }

    fn is_function(&self, item: ItemId) -> bool {
        self.item(item).kind == ItemKind::Function
    }

    fn ast_of(&self, item: ItemId) -> crate::AstId {
        self.item(item).ast
    }
}

/// The packages a package can see: itself, then the transitive closure of its dependencies in
/// breadth-first order (0015 §3.2).
pub struct VisiblePackages;
impl Query for VisiblePackages {
    type Key = PackageId;
    type Value = Arc<[PackageId]>;
    fn execute(cx: &Cx<'_>, package: &PackageId) -> Result<Arc<[PackageId]>, Cancelled> {
        let mut seen = HashSet::from([*package]);
        let mut order = vec![*package];
        let mut queue = VecDeque::from([*package]);
        while let Some(p) = queue.pop_front() {
            for &dep in cx.input::<PackageInput>(&p)?.dependencies.iter() {
                if seen.insert(dep) {
                    order.push(dep);
                    queue.push_back(dep);
                }
            }
        }
        Ok(order.into())
    }
}
