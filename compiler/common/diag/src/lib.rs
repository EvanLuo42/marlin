//! Structured diagnostics (`docs/design/0005-shader-compiler.md` §5.3, `0015-language-frontend.md` §2.3).
//!
//! The compiler never prints or logs. Every phase returns [`Diagnostic`] values: a code and
//! severity, a primary span with optional extra labels, notes, and suggested edits. Hosts turn
//! them into whatever they need; [`render`] produces the rustc-style text that build scripts and
//! tests show, and the cooker forwards the values to marlin-diag.

mod render;

use std::fmt;

pub use marlin_lang_span::{FileId, Span};
pub use render::{SourceMap, render, render_all};

/// How bad a diagnostic is. Any error fails the compile request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Warning,
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Warning => "warning",
            Self::Error => "error",
        })
    }
}

/// A stable diagnostic code such as `E0102`. The hundreds digit groups codes by phase: `E01xx`
/// lexing, `E02xx` parsing, and so on upward through the compiler.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Code(pub &'static str);

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// A span with a message shown under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

/// Whether a tool may apply a [`Suggestion`] without asking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applicability {
    /// The edit is certainly what the author meant; IDE quick fixes can apply it directly.
    MachineApplicable,
    /// The edit is a likely fix but needs a human to check it.
    MaybeIncorrect,
}

/// Replace `span` with `replacement`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub span: Span,
    pub replacement: String,
}

/// A fix the author can apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub message: String,
    pub edits: Vec<TextEdit>,
    pub applicability: Applicability,
}

/// One compiler message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Code,
    pub message: String,
    /// Where the problem is. `None` only for problems with no source location, such as a
    /// missing entry point named by the host.
    pub primary: Option<Label>,
    pub labels: Vec<Label>,
    pub notes: Vec<String>,
    pub suggestions: Vec<Suggestion>,
}

impl Diagnostic {
    pub fn new(severity: Severity, code: Code, message: impl Into<String>) -> Self {
        Self {
            severity,
            code,
            message: message.into(),
            primary: None,
            labels: Vec::new(),
            notes: Vec::new(),
            suggestions: Vec::new(),
        }
    }

    pub fn error(code: Code, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, code, message)
    }

    pub fn warning(code: Code, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, code, message)
    }

    /// Sets the primary span, with `message` shown under it (may be empty).
    pub fn at(mut self, span: Span, message: impl Into<String>) -> Self {
        self.primary = Some(Label {
            span,
            message: message.into(),
        });
        self
    }

    /// Adds a secondary label.
    pub fn label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            message: message.into(),
        });
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Adds a single-edit suggestion.
    pub fn suggest(
        mut self,
        message: impl Into<String>,
        span: Span,
        replacement: impl Into<String>,
        applicability: Applicability,
    ) -> Self {
        self.suggestions.push(Suggestion {
            message: message.into(),
            edits: vec![TextEdit {
                span,
                replacement: replacement.into(),
            }],
            applicability,
        });
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// Whether any diagnostic in `diagnostics` is an error.
pub fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(Diagnostic::is_error)
}
