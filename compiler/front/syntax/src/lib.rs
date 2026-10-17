//! The shared syntax layer of the C#-family languages (`docs/design/0015-language-frontend.md`):
//! a lexer, a lossless concrete syntax tree, an error-tolerant parser for the superset grammar,
//! validation against a [`Dialect`], and typed [`ast`] views.
//!
//! [`parse`] never fails. Whatever the input, it returns a tree whose text is exactly the input,
//! plus the problems found, so editors and the compiler work on broken code too.

pub mod ast;
mod dialect;
mod grammar;
mod kind;
mod lexer;
mod parser;
mod tree;
mod validate;

use std::sync::Arc;

use marlin_lang_diag::{Code, Diagnostic, FileId, Span};
use marlin_lang_span::TextRange;

pub use dialect::{Construct, ConstructSet, Dialect};
pub use kind::SyntaxKind;
pub use lexer::{LexError, Token, lex};
pub use tree::{
    GreenBuilder, GreenElement, GreenNode, GreenToken, SyntaxElement, SyntaxNode, SyntaxToken,
    WalkEvent,
};

/// Which phase found a [`SyntaxError`]; it decides the diagnostic code (0015 §2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SyntaxErrorKind {
    Lex,
    Parse,
    Validation,
}

impl SyntaxErrorKind {
    pub fn code(self) -> Code {
        match self {
            Self::Lex => Code("E0101"),
            Self::Parse => Code("E0201"),
            Self::Validation => Code("E0301"),
        }
    }
}

/// A problem found by the lexer, the parser or validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub kind: SyntaxErrorKind,
    pub range: TextRange,
    pub message: String,
    /// A suggestion shown as a note (validation errors).
    pub hint: Option<&'static str>,
}

impl SyntaxError {
    pub fn new(kind: SyntaxErrorKind, range: TextRange, message: impl Into<String>) -> Self {
        Self {
            kind,
            range,
            message: message.into(),
            hint: None,
        }
    }

    pub fn to_diagnostic(&self, file: FileId) -> Diagnostic {
        let mut d = Diagnostic::error(self.kind.code(), self.message.clone())
            .at(Span::new(file, self.range), "");
        if let Some(hint) = self.hint {
            d = d.note(hint);
        }
        d
    }
}

/// The result of parsing one file.
#[derive(Clone)]
pub struct Parse {
    green: Arc<GreenNode>,
    errors: Vec<SyntaxError>,
}

impl Parse {
    pub fn green(&self) -> &Arc<GreenNode> {
        &self.green
    }

    pub fn syntax_node(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }

    pub fn tree(&self) -> ast::SourceFile {
        ast::AstNode::cast(self.syntax_node()).expect("the root is a SOURCE_FILE")
    }

    /// Lexer, parser and validation errors, in source order.
    pub fn errors(&self) -> &[SyntaxError] {
        &self.errors
    }

    pub fn diagnostics(&self, file: FileId) -> Vec<Diagnostic> {
        self.errors.iter().map(|e| e.to_diagnostic(file)).collect()
    }

    /// The tree dump followed by one line per error; the format of the snapshot tests.
    pub fn debug_dump(&self) -> String {
        let mut out = self.syntax_node().debug_dump();
        for e in &self.errors {
            out.push_str(&format!("{} {:?}: {}\n", e.kind.code(), e.range, e.message));
            if let Some(hint) = e.hint {
                out.push_str(&format!("  note: {hint}\n"));
            }
        }
        out
    }
}

/// Parses `text` as one source file of `dialect`.
pub fn parse(text: &str, dialect: &Dialect) -> Parse {
    let (tokens, lex_errors) = lexer::lex(text, dialect.extra_keywords);
    let input = parser::Input::new(&tokens);
    let mut p = parser::Parser::new(text, &input);
    grammar::source_file(&mut p);
    let (green, parse_errors) = parser::build_tree(text, &tokens, p.finish());
    let validation = validate::validate(&SyntaxNode::new_root(green.clone()), dialect);

    let mut errors: Vec<SyntaxError> = lex_errors
        .into_iter()
        .map(|e| SyntaxError::new(SyntaxErrorKind::Lex, e.range, e.message))
        .chain(parse_errors)
        .chain(validation)
        .collect();
    errors.sort_by_key(|e| (e.range.start(), e.kind));
    // Nested constructs that stop at the same token report the same problem once.
    errors.dedup_by(|a, b| a.range == b.range && a.message == b.message);
    Parse { green, errors }
}
