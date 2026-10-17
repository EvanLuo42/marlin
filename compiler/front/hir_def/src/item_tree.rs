//! The declarations of one file, without bodies or positions.

use std::sync::Arc;

use marlin_front_syntax::ast::{self, AstNode, HasGenerics, HasModifiers, HasName};
use marlin_front_syntax::{SyntaxKind, SyntaxNode};
use marlin_lang_arena::{Arena, Idx};
use marlin_lang_query::{Cancelled, Cx, Query};
use marlin_lang_span::FileId;

use crate::ast_id::{AstId, AstIdMap};
use crate::{Name, ParseFile};

pub type LocalItemId = Idx<Item>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ItemTree {
    /// File-scoped `namespace` declarations; more than one is an error reported by the def map.
    pub namespace_decls: Vec<(Path, AstId)>,
    pub usings: Vec<Using>,
    /// Declarations outside any type, in source order.
    pub top_level: Vec<LocalItemId>,
    pub items: Arena<Item>,
}

impl ItemTree {
    /// The file's namespace: the first file-scoped declaration, or `None`.
    pub fn namespace(&self) -> Option<&Path> {
        self.namespace_decls.first().map(|(path, _)| path)
    }
}

/// A path as written: `global::A.B<T, U>.C`, keeping each segment's number of type arguments.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Path {
    pub global: bool,
    pub segments: Vec<PathSegment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PathSegment {
    pub name: Name,
    pub arity: u32,
}

impl Path {
    pub fn from_ast(path: &ast::Path) -> Option<Path> {
        let segments = path
            .segments()
            .map(|s| {
                Some(PathSegment {
                    name: Name::new(&s.name_ref()?.text()),
                    arity: s.generic_args().map_or(0, |a| a.count() as u32),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        (!segments.is_empty()).then(|| Path {
            global: path.is_global(),
            segments,
        })
    }

    /// Parses `A.B.C` (no type arguments), for tests and language descriptions.
    pub fn parse(text: &str) -> Path {
        Path {
            global: false,
            segments: text
                .split('.')
                .map(|name| PathSegment {
                    name: Name::new(name.trim()),
                    arity: 0,
                })
                .collect(),
        }
    }
}

impl std::fmt::Display for Path {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.global {
            f.write_str("global::")?;
        }
        for (i, s) in self.segments.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            write!(f, "{}", s.name)?;
            if s.arity > 0 {
                write!(f, "<{}>", ",".repeat(s.arity as usize - 1))?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Using {
    pub kind: UsingKind,
    pub global: bool,
    /// `None` when the source is too broken to have one.
    pub path: Option<Path>,
    pub ast: AstId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum UsingKind {
    /// `using A.B;`
    Namespace,
    /// `using static A.B.T;`
    Static,
    /// `using X = A.B.T;`
    Alias(Name),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Visibility {
    Private,
    Protected,
    Internal,
    Public,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemKind {
    /// A block-scoped `namespace A.B { ... }`.
    Namespace,
    Struct,
    Class,
    Record,
    Interface,
    Enum,
    EnumMember,
    Extension,
    Function,
    Constructor,
    Operator,
    Conversion,
    Property,
    Indexer,
    Field,
    AssocType,
}

impl ItemKind {
    /// Kinds that declare a named type.
    pub fn is_type(self) -> bool {
        matches!(
            self,
            Self::Struct | Self::Class | Self::Record | Self::Interface | Self::Enum
        )
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::Namespace => "namespace",
            Self::Struct => "struct",
            Self::Class => "class",
            Self::Record => "record",
            Self::Interface => "interface",
            Self::Enum => "enum",
            Self::EnumMember => "enum member",
            Self::Extension => "extension block",
            Self::Function => "function",
            Self::Constructor => "constructor",
            Self::Operator => "operator",
            Self::Conversion => "conversion",
            Self::Property => "property",
            Self::Indexer => "indexer",
            Self::Field => "field",
            Self::AssocType => "associated type",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GenericParam {
    pub name: Name,
    /// `const int N`
    pub is_const: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Item {
    pub kind: ItemKind,
    /// `None` for extension blocks, operators, conversions and indexers.
    pub name: Option<Name>,
    /// For a block namespace: its path.
    pub namespace: Option<Path>,
    pub visibility: Visibility,
    pub is_static: bool,
    /// `const` fields and functions.
    pub is_const: bool,
    pub generic_params: Vec<GenericParam>,
    pub members: Vec<LocalItemId>,
    pub parent: Option<LocalItemId>,
    pub ast: AstId,
    /// Which declarator of a field declaration (`float Min, Max;`) this is.
    pub declarator: u32,
}

impl Item {
    /// Number of type parameters.
    pub fn arity(&self) -> u32 {
        self.generic_params.len() as u32
    }
}

pub struct ItemTreeQuery;
impl Query for ItemTreeQuery {
    type Key = FileId;
    type Value = Arc<ItemTree>;
    fn execute(cx: &Cx<'_>, file: &FileId) -> Result<Arc<ItemTree>, Cancelled> {
        let parse = cx.get::<ParseFile>(file)?;
        let ids = cx.get::<AstIdMap>(file)?;
        Ok(Arc::new(lower(&parse.syntax_node(), &ids)))
    }
}

pub(crate) fn lower(root: &SyntaxNode, ids: &AstIdMap) -> ItemTree {
    let mut cx = Lower {
        ids,
        tree: ItemTree {
            namespace_decls: Vec::new(),
            usings: Vec::new(),
            top_level: Vec::new(),
            items: Arena::new(),
        },
    };
    for child in root.children() {
        match child.kind() {
            SyntaxKind::USING_DIRECTIVE => cx.using(&child),
            SyntaxKind::NAMESPACE_DECL
                if !child
                    .children()
                    .any(|c| c.kind() == SyntaxKind::MEMBER_LIST) =>
            {
                let ns = ast::NamespaceDecl::cast(child.clone()).unwrap();
                if let Some(path) = ns.path().as_ref().and_then(Path::from_ast) {
                    let id = cx.ids.id(&child);
                    cx.tree.namespace_decls.push((path, id));
                }
            }
            _ => {
                let items = cx.member(&child, None);
                cx.tree.top_level.extend(items);
            }
        }
    }
    cx.tree
}

struct Lower<'a> {
    ids: &'a AstIdMap,
    tree: ItemTree,
}

impl Lower<'_> {
    fn using(&mut self, node: &SyntaxNode) {
        let using = ast::UsingDirective::cast(node.clone()).unwrap();
        let kind = if let Some(alias) = using.alias() {
            match alias.name() {
                Some(name) => UsingKind::Alias(Name::new(&name.text())),
                None => return,
            }
        } else if using.is_static() {
            UsingKind::Static
        } else {
            UsingKind::Namespace
        };
        self.tree.usings.push(Using {
            kind,
            global: using.is_global(),
            path: using.path().as_ref().and_then(Path::from_ast),
            ast: self.ids.id(node),
        });
    }

    /// Lowers a declaration node; a field declaration gives one item per declarator.
    fn member(&mut self, node: &SyntaxNode, parent: Option<LocalItemId>) -> Vec<LocalItemId> {
        use SyntaxKind::*;
        let Some(member) = ast::Member::cast(node.clone()) else {
            return Vec::new();
        };
        let kind = match node.kind() {
            NAMESPACE_DECL => ItemKind::Namespace,
            STRUCT_DECL => ItemKind::Struct,
            CLASS_DECL => ItemKind::Class,
            RECORD_DECL => ItemKind::Record,
            INTERFACE_DECL => ItemKind::Interface,
            ENUM_DECL => ItemKind::Enum,
            EXTENSION_DECL => ItemKind::Extension,
            FUNCTION_DECL => ItemKind::Function,
            CONSTRUCTOR_DECL => ItemKind::Constructor,
            OPERATOR_DECL => ItemKind::Operator,
            CONVERSION_DECL => ItemKind::Conversion,
            PROPERTY_DECL => ItemKind::Property,
            INDEXER_DECL => ItemKind::Indexer,
            FIELD_DECL => ItemKind::Field,
            ASSOC_TYPE_DECL => ItemKind::AssocType,
            _ => return Vec::new(),
        };
        let ast = self.ids.id(node);
        let modifiers = modifiers_of(&member);
        let has = |k: SyntaxKind| modifiers.contains(&k);
        let explicit = [PUBLIC_KW, INTERNAL_KW, PRIVATE_KW, PROTECTED_KW]
            .into_iter()
            .find(|&k| has(k));
        let parent_kind = parent.map(|p| self.tree.items[p].kind);
        let visibility = match explicit {
            Some(PUBLIC_KW) => Visibility::Public,
            Some(INTERNAL_KW) => Visibility::Internal,
            Some(PRIVATE_KW) => Visibility::Private,
            Some(_) => Visibility::Protected,
            None => match parent_kind {
                None | Some(ItemKind::Namespace) => Visibility::Internal,
                Some(ItemKind::Interface | ItemKind::Enum) => Visibility::Public,
                Some(_) => Visibility::Private,
            },
        };
        let template = Item {
            kind,
            name: None,
            namespace: None,
            visibility,
            is_static: has(STATIC_KW),
            is_const: has(CONST_KW),
            generic_params: generic_params(&member),
            members: Vec::new(),
            parent,
            ast,
            declarator: 0,
        };

        if let ast::Member::FieldDecl(field) = &member {
            return field
                .declarators()
                .enumerate()
                .filter_map(|(i, d)| {
                    let name = Name::new(&d.name()?.text());
                    Some(self.tree.items.alloc(Item {
                        name: Some(name),
                        declarator: i as u32,
                        ..template.clone()
                    }))
                })
                .collect();
        }

        let name = match &member {
            ast::Member::NamespaceDecl(_) => None,
            _ => node
                .children()
                .find(|c| c.kind() == NAME)
                .and_then(ast::Name::cast)
                .map(|n| Name::new(&n.text())),
        };
        let namespace = match &member {
            ast::Member::NamespaceDecl(ns) => ns.path().as_ref().and_then(Path::from_ast),
            _ => None,
        };
        let id = self.tree.items.alloc(Item {
            name,
            namespace,
            ..template
        });

        let mut members = Vec::new();
        match &member {
            ast::Member::EnumDecl(e) => {
                for m in e.members() {
                    let Some(name) = m.name() else { continue };
                    members.push(self.tree.items.alloc(Item {
                        kind: ItemKind::EnumMember,
                        name: Some(Name::new(&name.text())),
                        namespace: None,
                        visibility: Visibility::Public,
                        is_static: true,
                        is_const: true,
                        generic_params: Vec::new(),
                        members: Vec::new(),
                        parent: Some(id),
                        ast: self.ids.id(m.syntax()),
                        declarator: 0,
                    }));
                }
            }
            _ => {
                if let Some(list) = node.children().find(|c| c.kind() == MEMBER_LIST) {
                    for child in list.children() {
                        members.extend(self.member(&child, Some(id)));
                    }
                }
            }
        }
        self.tree.items[id].members = members;
        vec![id]
    }
}

fn modifiers_of(member: &ast::Member) -> Vec<SyntaxKind> {
    macro_rules! mods {
        ($($variant:ident),*) => {
            match member {
                $(ast::Member::$variant(n) => n.modifiers(),)*
                ast::Member::NamespaceDecl(_) => None,
            }
        };
    }
    mods!(
        StructDecl,
        ClassDecl,
        RecordDecl,
        InterfaceDecl,
        EnumDecl,
        ExtensionDecl,
        FunctionDecl,
        ConstructorDecl,
        OperatorDecl,
        ConversionDecl,
        PropertyDecl,
        IndexerDecl,
        FieldDecl,
        AssocTypeDecl
    )
    .map(|m| m.kinds().collect())
    .unwrap_or_default()
}

fn generic_params(member: &ast::Member) -> Vec<GenericParam> {
    let list = match member {
        ast::Member::StructDecl(n) => n.type_param_list(),
        ast::Member::ClassDecl(n) => n.type_param_list(),
        ast::Member::RecordDecl(n) => n.type_param_list(),
        ast::Member::InterfaceDecl(n) => n.type_param_list(),
        ast::Member::ExtensionDecl(n) => n.type_param_list(),
        ast::Member::FunctionDecl(n) => n.type_param_list(),
        _ => None,
    };
    list.map(|l| {
        l.params()
            .filter_map(|p| {
                Some(GenericParam {
                    name: Name::new(&p.name()?.text()),
                    is_const: p.const_type().is_some(),
                })
            })
            .collect()
    })
    .unwrap_or_default()
}
