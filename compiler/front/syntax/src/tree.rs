//! The lossless concrete syntax tree.
//!
//! Two layers, as in rust-analyzer:
//! - the **green** tree is immutable, position-independent and shareable: a node is its kind,
//!   text length and children; a token is its kind and text;
//! - the **red** tree ([`SyntaxNode`], [`SyntaxToken`]) is a cursor over a green tree that adds
//!   parent links and absolute offsets, created on demand while walking.
//!
//! Every byte of the source, trivia included, is in exactly one token, so the text of the root
//! is the source text.

use std::fmt::{self, Write};
use std::sync::Arc;

use marlin_lang_span::{TextRange, TextSize};

use crate::SyntaxKind;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct GreenToken {
    kind: SyntaxKind,
    text: Box<str>,
}

impl GreenToken {
    pub fn new(kind: SyntaxKind, text: &str) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }

    pub fn kind(&self) -> SyntaxKind {
        self.kind
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn text_len(&self) -> TextSize {
        TextSize::of(&self.text)
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum GreenElement {
    Node(Arc<GreenNode>),
    Token(Arc<GreenToken>),
}

impl GreenElement {
    pub fn kind(&self) -> SyntaxKind {
        match self {
            Self::Node(n) => n.kind,
            Self::Token(t) => t.kind,
        }
    }

    pub fn text_len(&self) -> TextSize {
        match self {
            Self::Node(n) => n.text_len,
            Self::Token(t) => t.text_len(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct GreenNode {
    kind: SyntaxKind,
    text_len: TextSize,
    children: Box<[GreenElement]>,
}

impl GreenNode {
    pub fn new(kind: SyntaxKind, children: Vec<GreenElement>) -> Self {
        let text_len = TextSize(children.iter().map(|c| c.text_len().0).sum());
        Self {
            kind,
            text_len,
            children: children.into_boxed_slice(),
        }
    }

    pub fn kind(&self) -> SyntaxKind {
        self.kind
    }

    pub fn text_len(&self) -> TextSize {
        self.text_len
    }

    pub fn children(&self) -> &[GreenElement] {
        &self.children
    }
}

impl Drop for GreenNode {
    // Dropping deep trees recursively could overflow the stack; tear them down iteratively.
    fn drop(&mut self) {
        let mut stack: Vec<Arc<GreenNode>> = Vec::new();
        let take = |children: &mut Box<[GreenElement]>, stack: &mut Vec<Arc<GreenNode>>| {
            for child in std::mem::take(children).into_vec() {
                if let GreenElement::Node(node) = child {
                    stack.push(node);
                }
            }
        };
        take(&mut self.children, &mut stack);
        while let Some(node) = stack.pop() {
            if let Ok(mut node) = Arc::try_unwrap(node) {
                take(&mut node.children, &mut stack);
            }
        }
    }
}

/// Builds a green tree from start/token/finish calls.
#[derive(Default)]
pub struct GreenBuilder {
    parents: Vec<(SyntaxKind, usize)>,
    children: Vec<GreenElement>,
}

impl GreenBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_node(&mut self, kind: SyntaxKind) {
        self.parents.push((kind, self.children.len()));
    }

    pub fn token(&mut self, kind: SyntaxKind, text: &str) {
        self.children
            .push(GreenElement::Token(Arc::new(GreenToken::new(kind, text))));
    }

    /// # Panics
    /// Without a matching `start_node`.
    pub fn finish_node(&mut self) {
        let (kind, first) = self.parents.pop().expect("finish_node without start_node");
        let children = self.children.split_off(first);
        self.children
            .push(GreenElement::Node(Arc::new(GreenNode::new(kind, children))));
    }

    /// # Panics
    /// If nodes are still open or there is not exactly one root.
    pub fn finish(mut self) -> Arc<GreenNode> {
        assert!(self.parents.is_empty(), "unfinished nodes");
        assert_eq!(self.children.len(), 1, "expected exactly one root");
        match self.children.pop().unwrap() {
            GreenElement::Node(node) => node,
            GreenElement::Token(_) => panic!("root is a token"),
        }
    }
}

struct NodeData {
    green: Arc<GreenNode>,
    parent: Option<SyntaxNode>,
    index: u32,
    offset: TextSize,
}

/// A node of the red tree.
#[derive(Clone)]
pub struct SyntaxNode(Arc<NodeData>);

impl SyntaxNode {
    pub fn new_root(green: Arc<GreenNode>) -> Self {
        Self(Arc::new(NodeData {
            green,
            parent: None,
            index: 0,
            offset: TextSize(0),
        }))
    }

    pub fn kind(&self) -> SyntaxKind {
        self.0.green.kind
    }

    pub fn green(&self) -> &Arc<GreenNode> {
        &self.0.green
    }

    pub fn text_range(&self) -> TextRange {
        TextRange::at(self.0.offset, self.0.green.text_len)
    }

    /// The source text this node covers, trivia included.
    pub fn text(&self) -> String {
        let mut text = String::with_capacity(self.0.green.text_len.to_usize());
        for token in self.descendant_tokens() {
            text.push_str(token.text());
        }
        text
    }

    pub fn parent(&self) -> Option<SyntaxNode> {
        self.0.parent.clone()
    }

    /// This node and its ancestors, innermost first.
    pub fn ancestors(&self) -> impl Iterator<Item = SyntaxNode> {
        std::iter::successors(Some(self.clone()), SyntaxNode::parent)
    }

    /// Child nodes and tokens in order.
    pub fn children_with_tokens(&self) -> impl Iterator<Item = SyntaxElement> + '_ {
        let mut offset = self.0.offset;
        self.0
            .green
            .children
            .iter()
            .enumerate()
            .map(move |(index, green)| {
                let element = self.child_element(index as u32, green, offset);
                offset = offset + green.text_len();
                element
            })
    }

    /// Child nodes in order.
    pub fn children(&self) -> impl Iterator<Item = SyntaxNode> + '_ {
        self.children_with_tokens()
            .filter_map(SyntaxElement::into_node)
    }

    pub fn first_child(&self) -> Option<SyntaxNode> {
        self.children().next()
    }

    /// Child tokens in order (not the tokens of child nodes).
    pub fn child_tokens(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.children_with_tokens()
            .filter_map(SyntaxElement::into_token)
    }

    fn child_element(&self, index: u32, green: &GreenElement, offset: TextSize) -> SyntaxElement {
        match green {
            GreenElement::Node(node) => SyntaxElement::Node(SyntaxNode(Arc::new(NodeData {
                green: node.clone(),
                parent: Some(self.clone()),
                index,
                offset,
            }))),
            GreenElement::Token(token) => SyntaxElement::Token(SyntaxToken {
                parent: self.clone(),
                index,
                offset,
                green: token.clone(),
            }),
        }
    }

    fn child_at(&self, index: u32) -> Option<SyntaxElement> {
        let children = &self.0.green.children;
        let green = children.get(index as usize)?;
        let offset = children[..index as usize]
            .iter()
            .fold(self.0.offset, |o, c| o + c.text_len());
        Some(self.child_element(index, green, offset))
    }

    pub fn next_sibling_or_token(&self) -> Option<SyntaxElement> {
        self.parent()?.child_at(self.0.index + 1)
    }

    pub fn prev_sibling_or_token(&self) -> Option<SyntaxElement> {
        self.parent()?.child_at(self.0.index.checked_sub(1)?)
    }

    /// The following sibling node, skipping tokens.
    pub fn next_sibling(&self) -> Option<SyntaxNode> {
        let parent = self.parent()?;
        let mut index = self.0.index + 1;
        loop {
            match parent.child_at(index)? {
                SyntaxElement::Node(node) => return Some(node),
                SyntaxElement::Token(_) => index += 1,
            }
        }
    }

    /// All nodes in this subtree in preorder, starting with this one.
    pub fn descendants(&self) -> impl Iterator<Item = SyntaxNode> {
        self.preorder().filter_map(|event| match event {
            WalkEvent::Enter(SyntaxElement::Node(node)) => Some(node),
            _ => None,
        })
    }

    /// All tokens in this subtree in source order.
    pub fn descendant_tokens(&self) -> impl Iterator<Item = SyntaxToken> {
        self.preorder().filter_map(|event| match event {
            WalkEvent::Enter(SyntaxElement::Token(token)) => Some(token),
            _ => None,
        })
    }

    pub fn first_token(&self) -> Option<SyntaxToken> {
        self.descendant_tokens().next()
    }

    /// Enter/leave events for every element of the subtree, without recursion.
    pub fn preorder(&self) -> impl Iterator<Item = WalkEvent> {
        // (node, index of the next child, offset of the next child)
        let mut stack: Vec<(SyntaxNode, u32, TextSize)> = Vec::new();
        let mut start = Some(self.clone());
        std::iter::from_fn(move || {
            if let Some(root) = start.take() {
                stack.push((root.clone(), 0, root.0.offset));
                return Some(WalkEvent::Enter(SyntaxElement::Node(root)));
            }
            let (node, next, offset) = stack.last_mut()?;
            match node.0.green.children.get(*next as usize) {
                Some(green) => {
                    let element = node.child_element(*next, green, *offset);
                    *next += 1;
                    *offset = *offset + green.text_len();
                    if let SyntaxElement::Node(child) = &element {
                        stack.push((child.clone(), 0, child.0.offset));
                    }
                    Some(WalkEvent::Enter(element))
                }
                None => {
                    let (node, _, _) = stack.pop().unwrap();
                    Some(WalkEvent::Leave(node))
                }
            }
        })
    }

    /// The token that contains `offset`, or the one starting at it when `offset` sits between
    /// two tokens.
    pub fn token_at_offset(&self, offset: TextSize) -> Option<SyntaxToken> {
        let mut node = self.clone();
        'descend: loop {
            for child in node.clone().children_with_tokens() {
                let range = child.text_range();
                if range.contains(offset) {
                    match child {
                        SyntaxElement::Node(n) => {
                            node = n;
                            continue 'descend;
                        }
                        SyntaxElement::Token(t) => return Some(t),
                    }
                }
            }
            return None;
        }
    }

    /// An indented dump of the subtree: one line per node or token, with kinds and ranges.
    pub fn debug_dump(&self) -> String {
        let mut out = String::new();
        let mut depth = 0usize;
        for event in self.preorder() {
            match event {
                WalkEvent::Enter(element) => {
                    let _ = write!(out, "{:indent$}", "", indent = depth * 2);
                    match element {
                        SyntaxElement::Node(node) => {
                            let _ = writeln!(out, "{:?}@{:?}", node.kind(), node.text_range());
                            depth += 1;
                        }
                        SyntaxElement::Token(token) => {
                            let _ = writeln!(
                                out,
                                "{:?}@{:?} {:?}",
                                token.kind(),
                                token.text_range(),
                                token.text()
                            );
                        }
                    }
                }
                WalkEvent::Leave(_) => depth -= 1,
            }
        }
        out
    }
}

impl PartialEq for SyntaxNode {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0.green, &other.0.green) && self.0.offset == other.0.offset
    }
}

impl Eq for SyntaxNode {}

impl std::hash::Hash for SyntaxNode {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0.green).hash(state);
        self.0.offset.hash(state);
    }
}

impl fmt::Debug for SyntaxNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}@{:?}", self.kind(), self.text_range())
    }
}

impl fmt::Display for SyntaxNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for token in self.descendant_tokens() {
            f.write_str(token.text())?;
        }
        Ok(())
    }
}

/// A token of the red tree.
#[derive(Clone)]
pub struct SyntaxToken {
    parent: SyntaxNode,
    index: u32,
    offset: TextSize,
    green: Arc<GreenToken>,
}

impl SyntaxToken {
    pub fn kind(&self) -> SyntaxKind {
        self.green.kind
    }

    pub fn text(&self) -> &str {
        &self.green.text
    }

    pub fn text_range(&self) -> TextRange {
        TextRange::at(self.offset, self.green.text_len())
    }

    pub fn parent(&self) -> SyntaxNode {
        self.parent.clone()
    }

    pub fn next_sibling_or_token(&self) -> Option<SyntaxElement> {
        self.parent.child_at(self.index + 1)
    }

    pub fn prev_sibling_or_token(&self) -> Option<SyntaxElement> {
        self.parent.child_at(self.index.checked_sub(1)?)
    }
}

impl PartialEq for SyntaxToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.green, &other.green) && self.offset == other.offset
    }
}

impl Eq for SyntaxToken {}

impl fmt::Debug for SyntaxToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?}@{:?} {:?}",
            self.kind(),
            self.text_range(),
            self.text()
        )
    }
}

impl fmt::Display for SyntaxToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyntaxElement {
    Node(SyntaxNode),
    Token(SyntaxToken),
}

impl SyntaxElement {
    pub fn kind(&self) -> SyntaxKind {
        match self {
            Self::Node(n) => n.kind(),
            Self::Token(t) => t.kind(),
        }
    }

    pub fn text_range(&self) -> TextRange {
        match self {
            Self::Node(n) => n.text_range(),
            Self::Token(t) => t.text_range(),
        }
    }

    pub fn into_node(self) -> Option<SyntaxNode> {
        match self {
            Self::Node(n) => Some(n),
            Self::Token(_) => None,
        }
    }

    pub fn into_token(self) -> Option<SyntaxToken> {
        match self {
            Self::Node(_) => None,
            Self::Token(t) => Some(t),
        }
    }
}

#[derive(Clone, Debug)]
pub enum WalkEvent {
    Enter(SyntaxElement),
    Leave(SyntaxNode),
}
