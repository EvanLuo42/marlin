//! Name lookup with C#'s rules (0004 §7.1, 0015 §3.3).

use marlin_lang_query::{Cancelled, Cx};

use crate::def_map::item_data;
use crate::imports::{AliasTarget, Imports, PackageImports, file_namespace};
use crate::item_tree::{ItemKind, Path, PathSegment, Visibility};
use crate::{
    FileId, FileImports, FilePackage, ItemId, ItemTreeQuery, NamespacePath, PackageDefMap,
    PackageId, VisiblePackages,
};

/// What a path names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    Namespace(NamespacePath),
    Type(ItemId),
    /// The `index`th type parameter of `owner`.
    TypeParam {
        owner: ItemId,
        index: u32,
    },
    /// Functions (an overload set), or a field.
    Values(Vec<ItemId>),
    /// The first `resolved` segments name `base` (a type or type parameter); the rest are
    /// members that need types to look up (`M.Params`, `T.Zero`).
    Partial {
        base: Box<Resolution>,
        resolved: usize,
    },
    /// Nothing named the segment at index `segment`.
    Unresolved {
        segment: usize,
    },
    /// Only declarations that are not accessible from here named the segment.
    Inaccessible {
        segment: usize,
        items: Vec<ItemId>,
    },
    /// Several declarations, none closer than the others, named the segment.
    Ambiguous {
        segment: usize,
        candidates: Vec<Resolution>,
    },
}

/// A namespace or type, as `using` directives and path prefixes need.
pub(crate) enum Target {
    Namespace(NamespacePath),
    Type(ItemId),
}

/// Where names are looked up from: a package, a file, its namespace, and the declarations
/// around the place (outermost first).
#[derive(Clone, Debug)]
pub struct Resolver {
    package: PackageId,
    file: FileId,
    namespace: NamespacePath,
    /// Enclosing types, extension blocks and functions, outermost first.
    enclosing: Vec<ItemId>,
}

impl Resolver {
    /// Lookup from inside `item`: its body, its signature, or its members.
    pub fn for_item(cx: &Cx<'_>, item: ItemId) -> Result<Resolver, Cancelled> {
        let tree = cx.get::<ItemTreeQuery>(&item.file)?;
        let mut chain = Vec::new();
        let mut namespace_tail = Vec::new();
        let mut at = Some(item.local);
        while let Some(local) = at {
            let data = &tree.items[local];
            match data.kind {
                ItemKind::Namespace => {
                    let segments = data.namespace.iter().flat_map(|p| &p.segments);
                    namespace_tail.splice(0..0, segments.map(|s| s.name.clone()));
                }
                _ => chain.push(ItemId {
                    file: item.file,
                    local,
                }),
            }
            at = data.parent;
        }
        chain.reverse();
        let mut namespace = file_namespace(&tree);
        for name in &namespace_tail {
            namespace = namespace.child(name);
        }
        Ok(Resolver {
            package: cx.input::<FilePackage>(&item.file)?,
            file: item.file,
            namespace,
            enclosing: chain,
        })
    }

    /// Lookup at a file's top level (outside any declaration).
    pub fn for_file(cx: &Cx<'_>, file: FileId) -> Result<Resolver, Cancelled> {
        let tree = cx.get::<ItemTreeQuery>(&file)?;
        Ok(Resolver {
            package: cx.input::<FilePackage>(&file)?,
            file,
            namespace: file_namespace(&tree),
            enclosing: Vec::new(),
        })
    }

    pub fn namespace(&self) -> &NamespacePath {
        &self.namespace
    }

    /// Resolves `path`. With `values`, the last segment may also name functions or fields (an
    /// expression position); otherwise only namespaces and types (a type position).
    pub fn resolve(&self, cx: &Cx<'_>, path: &Path, values: bool) -> Result<Resolution, Cancelled> {
        let Some(first) = path.segments.first() else {
            return Ok(Resolution::Unresolved { segment: 0 });
        };
        let last = path.segments.len() == 1;
        let mut current = if path.global {
            self.in_namespace_level(cx, &NamespacePath::root(), first, values && last, None)?
                .unwrap_or(Resolution::Unresolved { segment: 0 })
        } else {
            self.first_segment(cx, first, values && last)?
        };
        for (i, segment) in path.segments.iter().enumerate().skip(1) {
            let last = i + 1 == path.segments.len();
            current = match current {
                Resolution::Namespace(ns) => {
                    self.in_namespace(cx, &ns, segment, values && last, i)?
                }
                Resolution::Type(owner) => {
                    match self.in_type(cx, owner, segment, values && last, i)? {
                        Some(found) => found,
                        None => Resolution::Partial {
                            base: Box::new(Resolution::Type(owner)),
                            resolved: i,
                        },
                    }
                }
                base @ Resolution::TypeParam { .. } => Resolution::Partial {
                    base: Box::new(base),
                    resolved: i,
                },
                done => return Ok(done),
            };
        }
        Ok(current)
    }

    /// The first segment: type parameters and members of the enclosing declarations, then the
    /// namespaces from the innermost out, each with its usings.
    fn first_segment(
        &self,
        cx: &Cx<'_>,
        segment: &PathSegment,
        values: bool,
    ) -> Result<Resolution, Cancelled> {
        for &item in self.enclosing.iter().rev() {
            let data = item_data(cx, item)?;
            if segment.arity == 0
                && let Some(index) = data
                    .generic_params
                    .iter()
                    .position(|p| p.name == segment.name)
            {
                return Ok(Resolution::TypeParam {
                    owner: item,
                    index: index as u32,
                });
            }
            if (data.kind.is_type() || data.kind == ItemKind::Extension)
                && let Some(found) = self.in_type(cx, item, segment, values, 0)?
            {
                return Ok(found);
            }
        }

        let file_imports = cx.get::<FileImports>(&self.file)?;
        let package_imports = cx.get::<PackageImports>(&self.package)?;
        let mut inaccessible = None;
        for namespace in self.namespace.ancestors() {
            let mut levels: Vec<&Imports> = Vec::new();
            if namespace == self.namespace {
                levels.push(&file_imports);
            }
            if namespace.is_root() {
                levels.push(&package_imports);
            }
            if let Some(found) =
                self.in_namespace_level(cx, &namespace, segment, values, Some(&levels))?
            {
                match found {
                    Resolution::Inaccessible { .. } => {
                        inaccessible.get_or_insert(found);
                    }
                    found => return Ok(found),
                }
            }
        }
        Ok(inaccessible.unwrap_or(Resolution::Unresolved { segment: 0 }))
    }

    /// One namespace level: its members, then (if given) the usings that apply there.
    fn in_namespace_level(
        &self,
        cx: &Cx<'_>,
        namespace: &NamespacePath,
        segment: &PathSegment,
        values: bool,
        usings: Option<&[&Imports]>,
    ) -> Result<Option<Resolution>, Cancelled> {
        let members = self.in_namespace(cx, namespace, segment, values, 0)?;
        if !matches!(members, Resolution::Unresolved { .. }) {
            return Ok(Some(members));
        }
        let mut found: Vec<Resolution> = Vec::new();
        let mut hidden = Vec::new();
        for imports in usings.unwrap_or_default() {
            if segment.arity == 0
                && let Some(target) = imports.aliases.get(&segment.name)
            {
                found.push(match target {
                    AliasTarget::Namespace(ns) => Resolution::Namespace(ns.clone()),
                    AliasTarget::Type(item) => Resolution::Type(*item),
                });
                continue;
            }
            for ns in &imports.namespaces {
                // A using imports the types (and functions) of a namespace, not its namespaces.
                let types = namespace_types(cx, self.package, ns, segment)?;
                self.collect_accessible(cx, types, &mut found, &mut hidden, Resolution::Type)?;
                if values {
                    let items = namespace_values(cx, self.package, ns, &segment.name)?;
                    self.collect_values(cx, items, &mut found, &mut hidden)?;
                }
            }
            for &owner in &imports.statics {
                if let Some(r) = self.in_type(cx, owner, segment, values, 0)? {
                    found.push(r);
                }
            }
        }
        found.dedup();
        Ok(match found.len() {
            0 if !hidden.is_empty() => Some(Resolution::Inaccessible {
                segment: 0,
                items: hidden,
            }),
            0 => None,
            1 => found.pop(),
            _ => Some(merge_or_ambiguous(found, 0)),
        })
    }

    /// `segment` inside namespace `ns`: a child namespace, a type, or (with `values`) functions
    /// and fields, across the visible packages.
    fn in_namespace(
        &self,
        cx: &Cx<'_>,
        ns: &NamespacePath,
        segment: &PathSegment,
        values: bool,
        index: usize,
    ) -> Result<Resolution, Cancelled> {
        let mut found = Vec::new();
        let mut hidden = Vec::new();
        if segment.arity == 0 {
            let child = ns.child(&segment.name);
            if namespace_exists(cx, self.package, &child)? {
                found.push(Resolution::Namespace(child));
            }
        }
        let types = namespace_types(cx, self.package, ns, segment)?;
        self.collect_accessible(cx, types, &mut found, &mut hidden, Resolution::Type)?;
        if values {
            let items = namespace_values(cx, self.package, ns, &segment.name)?;
            self.collect_values(cx, items, &mut found, &mut hidden)?;
        }
        Ok(match found.len() {
            0 if !hidden.is_empty() => Resolution::Inaccessible {
                segment: index,
                items: hidden,
            },
            0 => Resolution::Unresolved { segment: index },
            1 => found.pop().unwrap(),
            _ => merge_or_ambiguous(found, index),
        })
    }

    /// A member of type (or extension block) `owner` named `segment`: a nested type or
    /// associated type, or with `values` its fields and functions. `None` when it has none,
    /// which for a type may still be a member found through types (`hir_ty`).
    fn in_type(
        &self,
        cx: &Cx<'_>,
        owner: ItemId,
        segment: &PathSegment,
        values: bool,
        index: usize,
    ) -> Result<Option<Resolution>, Cancelled> {
        let tree = cx.get::<ItemTreeQuery>(&owner.file)?;
        let mut types = Vec::new();
        let mut items = Vec::new();
        for &local in &tree.items[owner.local].members {
            let data = &tree.items[local];
            if data.name.as_ref() != Some(&segment.name) {
                continue;
            }
            let id = ItemId {
                file: owner.file,
                local,
            };
            if data.kind.is_type() || data.kind == ItemKind::AssocType {
                if data.arity() == segment.arity {
                    types.push(id);
                }
            } else if values && segment.arity == 0 && data.kind != ItemKind::Constructor {
                items.push(id);
            }
        }
        let mut found = Vec::new();
        let mut hidden = Vec::new();
        self.collect_accessible(cx, types, &mut found, &mut hidden, Resolution::Type)?;
        self.collect_values(cx, items, &mut found, &mut hidden)?;
        Ok(match found.len() {
            0 if !hidden.is_empty() => Some(Resolution::Inaccessible {
                segment: index,
                items: hidden,
            }),
            0 => None,
            1 => found.pop(),
            _ => Some(merge_or_ambiguous(found, index)),
        })
    }

    fn collect_accessible(
        &self,
        cx: &Cx<'_>,
        items: Vec<ItemId>,
        found: &mut Vec<Resolution>,
        hidden: &mut Vec<ItemId>,
        wrap: fn(ItemId) -> Resolution,
    ) -> Result<(), Cancelled> {
        for item in items {
            if self.accessible(cx, item)? {
                found.push(wrap(item));
            } else {
                hidden.push(item);
            }
        }
        Ok(())
    }

    /// Accessible functions and fields go into one `Values` result.
    fn collect_values(
        &self,
        cx: &Cx<'_>,
        items: Vec<ItemId>,
        found: &mut Vec<Resolution>,
        hidden: &mut Vec<ItemId>,
    ) -> Result<(), Cancelled> {
        let mut values = Vec::new();
        for item in items {
            if self.accessible(cx, item)? {
                values.push(item);
            } else {
                hidden.push(item);
            }
        }
        if !values.is_empty() {
            found.push(Resolution::Values(values));
        }
        Ok(())
    }

    /// Whether code at this resolver's position may use `item` (C#'s accessibility domains,
    /// with `protected` as `private` since there is no inheritance between structs yet).
    pub fn accessible(&self, cx: &Cx<'_>, item: ItemId) -> Result<bool, Cancelled> {
        let tree = cx.get::<ItemTreeQuery>(&item.file)?;
        let same_package = cx.input::<FilePackage>(&item.file)? == self.package;
        let mut at = Some(item.local);
        while let Some(local) = at {
            let data = &tree.items[local];
            let parent = data
                .parent
                .filter(|&p| tree.items[p].kind != ItemKind::Namespace);
            let ok = match data.visibility {
                Visibility::Public => true,
                Visibility::Internal => same_package,
                Visibility::Private | Visibility::Protected => match parent {
                    // Namespace-level `private` acts as `internal`.
                    None => same_package,
                    Some(p) => self.enclosing.contains(&ItemId {
                        file: item.file,
                        local: p,
                    }),
                },
            };
            if !ok {
                return Ok(false);
            }
            at = parent;
        }
        Ok(true)
    }
}

/// Functions found in several places form one overload set; anything else found more than
/// once is ambiguous.
fn merge_or_ambiguous(found: Vec<Resolution>, segment: usize) -> Resolution {
    if found.iter().all(|r| matches!(r, Resolution::Values(_))) {
        let mut items: Vec<ItemId> = found
            .into_iter()
            .flat_map(|r| match r {
                Resolution::Values(items) => items,
                _ => unreachable!(),
            })
            .collect();
        items.sort();
        items.dedup();
        return Resolution::Values(items);
    }
    Resolution::Ambiguous {
        segment,
        candidates: found,
    }
}

pub(crate) fn namespace_exists(
    cx: &Cx<'_>,
    package: PackageId,
    ns: &NamespacePath,
) -> Result<bool, Cancelled> {
    for &p in cx.get::<VisiblePackages>(&package)?.iter() {
        if cx.get::<PackageDefMap>(&p)?.namespace(ns).is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Types named `segment` (with its number of type arguments) in `ns`, over the visible
/// packages, before any accessibility check.
pub(crate) fn namespace_types(
    cx: &Cx<'_>,
    package: PackageId,
    ns: &NamespacePath,
    segment: &PathSegment,
) -> Result<Vec<ItemId>, Cancelled> {
    let mut out = Vec::new();
    for &p in cx.get::<VisiblePackages>(&package)?.iter() {
        let map = cx.get::<PackageDefMap>(&p)?;
        if let Some(items) = map
            .namespace(ns)
            .and_then(|s| s.types.get(&(segment.name.clone(), segment.arity)))
        {
            // A duplicate within a package is reported there; lookup sees the first.
            out.push(items[0]);
        }
    }
    Ok(out)
}

pub(crate) fn namespace_values(
    cx: &Cx<'_>,
    package: PackageId,
    ns: &NamespacePath,
    name: &crate::Name,
) -> Result<Vec<ItemId>, Cancelled> {
    let mut out = Vec::new();
    for &p in cx.get::<VisiblePackages>(&package)?.iter() {
        let map = cx.get::<PackageDefMap>(&p)?;
        if let Some(items) = map.namespace(ns).and_then(|s| s.values.get(name)) {
            out.extend(items.iter().copied());
        }
    }
    Ok(out)
}

/// Resolves the path of a `using` (or any path that must name a namespace or type) from
/// `start` outward, without consulting usings and checking only package visibility.
pub(crate) fn resolve_absolute(
    cx: &Cx<'_>,
    package: PackageId,
    start: &NamespacePath,
    path: &Path,
) -> Result<Option<Target>, Cancelled> {
    let resolver = Resolver {
        package,
        file: FileId(u32::MAX),
        namespace: start.clone(),
        enclosing: Vec::new(),
    };
    let Some(first) = path.segments.first() else {
        return Ok(None);
    };
    let starts: Vec<NamespacePath> = if path.global {
        vec![NamespacePath::root()]
    } else {
        start.ancestors().collect()
    };
    let mut current = None;
    for ns in starts {
        match resolver.in_namespace(cx, &ns, first, false, 0)? {
            Resolution::Namespace(n) => current = Some(Target::Namespace(n)),
            Resolution::Type(t) => current = Some(Target::Type(t)),
            _ => continue,
        }
        break;
    }
    for segment in &path.segments[1..] {
        current = match current {
            Some(Target::Namespace(ns)) => {
                match resolver.in_namespace(cx, &ns, segment, false, 0)? {
                    Resolution::Namespace(n) => Some(Target::Namespace(n)),
                    Resolution::Type(t) => Some(Target::Type(t)),
                    _ => None,
                }
            }
            Some(Target::Type(owner)) => match resolver.in_type(cx, owner, segment, false, 0)? {
                Some(Resolution::Type(t)) => Some(Target::Type(t)),
                _ => None,
            },
            None => None,
        };
    }
    Ok(current)
}

/// The full name of a declaration: `Marlin.Post.Blur`, `Marlin.Light.Color`.
pub fn item_path(cx: &Cx<'_>, item: ItemId) -> Result<String, Cancelled> {
    let resolver = Resolver::for_item(cx, item)?;
    let tree = cx.get::<ItemTreeQuery>(&item.file)?;
    let mut names = Vec::new();
    let mut at = Some(item.local);
    while let Some(local) = at {
        let data = &tree.items[local];
        if data.kind != ItemKind::Namespace {
            names.push(match &data.name {
                Some(name) => name.to_string(),
                None => format!("<{}>", data.kind.describe()),
            });
        }
        at = data.parent;
    }
    names.reverse();
    let ns = resolver.namespace();
    Ok(if ns.is_root() {
        names.join(".")
    } else {
        format!("{ns}.{}", names.join("."))
    })
}
