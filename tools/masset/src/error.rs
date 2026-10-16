//! Errors and diagnostics.

use crate::ast::{Location, Span};

/// Text that is not valid `.masset` syntax. Parsing stops at the first one.
#[derive(Clone, PartialEq, Eq, Debug, marlin_error::Error)]
#[error("{location}: {message}")]
pub struct ParseError {
    pub message: String,
    pub span: Span,
    pub location: Location,
}

impl ParseError {
    pub(crate) fn new(text: &str, span: Span, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span,
            location: span.location(text),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Severity {
    /// The value could not be read; the field keeps its default.
    Error,
    /// Read, but with something to fix: an unknown field (kept as is), a missing link UUID.
    Warning,
}

/// A problem found while binding text to types. Binding keeps going after one, so a file reports
/// everything wrong with it at once.
#[derive(Clone, PartialEq, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Span,
}

impl Diagnostic {
    pub(crate) fn error(span: crate::ast::Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            span,
        }
    }

    pub(crate) fn warning(span: crate::ast::Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            span,
        }
    }

    /// `line:col: message`, with the position resolved against `text`.
    pub fn display(&self, text: &str) -> String {
        let kind = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        format!("{}: {kind}: {}", self.span.location(text), self.message)
    }
}

/// A type that cannot be used in source text.
#[derive(Clone, PartialEq, Eq, Debug, marlin_error::Error)]
pub enum SchemaError {
    #[error(
        "type `{name}` is registered twice (`{first}` and `{second}`); give one a unique #[reflect(name = \"...\")]"
    )]
    DuplicateType {
        name: String,
        first: String,
        second: String,
    },
    #[error("fields `{first}` and `{second}` of `{ty}` are both `{pascal}` in source text")]
    FieldCollision {
        ty: String,
        first: String,
        second: String,
        pascal: String,
    },
    #[error("`{0}` is generic; object types cannot be")]
    Generic(String),
    #[error("`{0}` is not a struct; object types must be")]
    NotStruct(String),
}
