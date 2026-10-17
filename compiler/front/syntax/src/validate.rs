//! Validation: rejects the constructs a dialect lacks (`docs/design/0015-language-frontend.md`
//! §2.1). The tree is the same for every dialect; only these diagnostics differ.

use marlin_lang_span::{TextRange, TextSize};

use crate::SyntaxKind::*;
use crate::dialect::{Construct, Dialect};
use crate::tree::{SyntaxNode, SyntaxToken};
use crate::{SyntaxError, SyntaxErrorKind, SyntaxKind};

pub(crate) fn validate(root: &SyntaxNode, dialect: &Dialect) -> Vec<SyntaxError> {
    let mut found: Vec<(Construct, TextRange)> = Vec::new();
    for node in root.descendants() {
        constructs_of(&node, &mut found);
    }
    found
        .into_iter()
        .filter(|(construct, _)| !dialect.allowed.contains(*construct))
        .map(|(construct, range)| {
            let mut error = SyntaxError::new(
                SyntaxErrorKind::Validation,
                range,
                format!("{} has no {}", dialect.name, construct.describe()),
            );
            error.hint = (dialect.hint)(construct);
            error
        })
        .collect()
}

fn token(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    node.child_tokens().find(|t| t.kind() == kind)
}

/// The range of `node`'s `kind` token, or of the whole node.
fn at_token(node: &SyntaxNode, kind: SyntaxKind) -> TextRange {
    token(node, kind).map_or_else(|| node.text_range(), |t| t.text_range())
}

/// Whether a member list belongs to a namespace (or the file), not a type.
fn is_namespace_level(node: &SyntaxNode) -> bool {
    match node.parent() {
        Some(parent) if parent.kind() == SOURCE_FILE => true,
        Some(parent) if parent.kind() == MEMBER_LIST => {
            parent.parent().is_some_and(|p| p.kind() == NAMESPACE_DECL)
        }
        _ => false,
    }
}

fn constructs_of(node: &SyntaxNode, out: &mut Vec<(Construct, TextRange)>) {
    let mut push = |c: Construct, range: TextRange| out.push((c, range));
    if matches!(
        node.kind(),
        STRUCT_DECL | CLASS_DECL | RECORD_DECL | INTERFACE_DECL
    ) && let Some(params) = node.children().find(|n| n.kind() == PARAM_LIST)
    {
        push(Construct::PrimaryConstructor, params.text_range());
    }
    match node.kind() {
        NEW_EXPR => push(Construct::New, at_token(node, NEW_KW)),
        CLASS_DECL => push(Construct::Class, at_token(node, CLASS_KW)),
        RECORD_DECL => push(Construct::Record, at_token(node, RECORD_KW)),
        AS_EXPR => push(Construct::As, at_token(node, AS_KW)),
        TYPEOF_EXPR => push(Construct::Typeof, at_token(node, TYPEOF_KW)),
        SIZEOF_EXPR => push(Construct::Sizeof, at_token(node, SIZEOF_KW)),
        BASE_EXPR => push(Construct::Base, node.text_range()),
        THROW_EXPR => push(Construct::Throw, at_token(node, THROW_KW)),
        LITERAL => {
            if let Some(t) = node.first_token() {
                match t.kind() {
                    NULL_KW => push(Construct::Null, t.text_range()),
                    CHAR => push(Construct::CharLiteral, t.text_range()),
                    FLOAT_NUMBER => {
                        if let Some(c) = literal_suffix(t.text()) {
                            push(c, t.text_range());
                        }
                    }
                    _ => {}
                }
            }
        }
        NAMESPACE_DECL => {
            if let Some(list) = node.children().find(|n| n.kind() == MEMBER_LIST) {
                let start = node.text_range().start();
                let end = list.text_range().start().max(start);
                push(
                    Construct::BlockNamespace,
                    TextRange::new(start, trim_end(node, end)),
                );
            }
        }
        MODIFIERS => {
            for t in node.child_tokens() {
                if let Some(c) = Construct::modifier(t.kind()) {
                    push(c, t.text_range());
                }
            }
        }
        IF_STMT => {
            if let Some(t) = token(node, CONST_KW) {
                push(Construct::ConstIf, t.text_range());
            }
        }
        ASSOC_TYPE_DECL if is_namespace_level(node) => {
            push(Construct::TypeAlias, at_token(node, TYPE_KW))
        }
        ASSOC_TYPE_DECL => push(Construct::AssocType, at_token(node, TYPE_KW)),
        TYPE_PARAM => {
            if let Some(t) = token(node, CONST_KW) {
                push(Construct::ConstGenericParam, t.text_range());
            }
        }
        EXTENSION_DECL => {
            push(Construct::Extension, at_token(node, EXTENSION_KW));
            if let Some(bases) = node.children().find(|n| n.kind() == BASE_LIST) {
                push(Construct::ExtensionConformance, bases.text_range());
            }
        }
        FUNCTION_DECL if is_namespace_level(node) => {
            let range = node
                .children()
                .find(|n| n.kind() == NAME)
                .map_or_else(|| node.text_range(), |n| n.text_range());
            push(Construct::NamespaceFunction, range);
        }
        FIELD_DECL if is_namespace_level(node) => {
            let range = node
                .children()
                .find(|n| n.kind() == VARIABLE_DECLARATOR)
                .map_or_else(|| node.text_range(), |n| n.text_range());
            push(Construct::NamespaceField, range);
        }
        _ => {}
    }
}

/// `1.5h` is a half literal, `1.5m` a decimal literal.
fn literal_suffix(text: &str) -> Option<Construct> {
    if text.starts_with("0x") || text.starts_with("0X") {
        return None;
    }
    match text.as_bytes().last()? {
        b'h' | b'H' => Some(Construct::HalfLiteral),
        b'm' | b'M' => Some(Construct::DecimalLiteral),
        _ => None,
    }
}

/// `end`, moved back over trailing whitespace inside `node`.
fn trim_end(node: &SyntaxNode, end: TextSize) -> TextSize {
    let mut last = node.text_range().start();
    for t in node.descendant_tokens() {
        if t.text_range().end() > end {
            break;
        }
        if !t.kind().is_trivia() {
            last = t.text_range().end();
        }
    }
    last
}
