//! Stable ids for declarations: their position in a preorder walk of the declarations, which
//! does not change when text inside bodies, or between declarations, changes.

use std::collections::HashMap;
use std::sync::Arc;

use marlin_front_syntax::{SyntaxKind, SyntaxNode};
use marlin_lang_query::{Cancelled, Cx, Query};
use marlin_lang_span::{FileId, TextRange};

use crate::ParseFile;

/// The `n`th declaration (or `using`) of a file, in preorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AstId(pub u32);

/// Where a node is in a file's current tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AstPtr {
    pub kind: SyntaxKind,
    pub range: TextRange,
}

impl AstPtr {
    pub fn new(node: &SyntaxNode) -> Self {
        Self {
            kind: node.kind(),
            range: node.text_range(),
        }
    }

    /// The node in `root`, which must be the tree this pointer was taken from.
    pub fn to_node(&self, root: &SyntaxNode) -> SyntaxNode {
        let mut node = root.clone();
        'descend: loop {
            if node.text_range() == self.range && node.kind() == self.kind {
                return node;
            }
            for child in node.clone().children() {
                if child.text_range().contains_range(self.range) {
                    node = child;
                    continue 'descend;
                }
            }
            panic!("no {:?} at {:?} in this tree", self.kind, self.range);
        }
    }
}

/// Declarations that get an id.
fn has_id(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        USING_DIRECTIVE
            | NAMESPACE_DECL
            | STRUCT_DECL
            | CLASS_DECL
            | RECORD_DECL
            | INTERFACE_DECL
            | ENUM_DECL
            | ENUM_MEMBER
            | EXTENSION_DECL
            | FUNCTION_DECL
            | CONSTRUCTOR_DECL
            | OPERATOR_DECL
            | CONVERSION_DECL
            | PROPERTY_DECL
            | INDEXER_DECL
            | FIELD_DECL
            | ASSOC_TYPE_DECL
    )
}

/// Nodes whose children may be declarations. Bodies are not among them, so local functions get
/// no id here (they belong to their function's body).
fn holds_declarations(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        SOURCE_FILE
            | NAMESPACE_DECL
            | MEMBER_LIST
            | STRUCT_DECL
            | CLASS_DECL
            | RECORD_DECL
            | INTERFACE_DECL
            | EXTENSION_DECL
            | ENUM_DECL
            | ENUM_MEMBER_LIST
    )
}

/// The declarations of one file, numbered.
#[derive(Debug, PartialEq, Eq)]
pub struct AstIdMap {
    ptrs: Vec<AstPtr>,
    ids: HashMap<AstPtr, AstId>,
}

impl AstIdMap {
    pub fn from_root(root: &SyntaxNode) -> Self {
        let mut ptrs = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(node) = stack.pop() {
            if has_id(node.kind()) {
                ptrs.push(AstPtr::new(&node));
            }
            if holds_declarations(node.kind()) {
                let children: Vec<_> = node.children().collect();
                stack.extend(children.into_iter().rev());
            }
        }
        let ids = ptrs
            .iter()
            .enumerate()
            .map(|(i, &p)| (p, AstId(i as u32)))
            .collect();
        Self { ptrs, ids }
    }

    pub fn ptr(&self, id: AstId) -> AstPtr {
        self.ptrs[id.0 as usize]
    }

    /// The id of a declaration node of this file's current tree.
    ///
    /// # Panics
    /// If `node` is not a numbered declaration.
    pub fn id(&self, node: &SyntaxNode) -> AstId {
        *self
            .ids
            .get(&AstPtr::new(node))
            .unwrap_or_else(|| panic!("{node:?} has no AstId"))
    }

    pub fn len(&self) -> usize {
        self.ptrs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ptrs.is_empty()
    }
}

impl Query for AstIdMap {
    type Key = FileId;
    type Value = Arc<AstIdMap>;
    fn execute(cx: &Cx<'_>, file: &FileId) -> Result<Arc<AstIdMap>, Cancelled> {
        let parse = cx.get::<ParseFile>(file)?;
        Ok(Arc::new(AstIdMap::from_root(&parse.syntax_node())))
    }
}
