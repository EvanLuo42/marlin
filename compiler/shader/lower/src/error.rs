//! Lowering errors: code the type checker accepted but lowering cannot (yet) handle, failed
//! compile-time evaluation, and cancellation.

use std::fmt;

use marlin_front_hir_def::BodyOwner;
use marlin_lang_query::{Cancelled, Cx};
use marlin_shaderc_hir_ty::{InferDiagnostic, InferLoc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LowerError {
    Cancelled(Cancelled),
    /// E13xx, at a place in a body when there is one.
    Error {
        code: &'static str,
        message: String,
        at: Option<(BodyOwner, InferLoc)>,
    },
}

impl From<Cancelled> for LowerError {
    fn from(c: Cancelled) -> Self {
        LowerError::Cancelled(c)
    }
}

impl LowerError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        LowerError::Error {
            code,
            message: message.into(),
            at: None,
        }
    }

    pub(crate) fn at(
        code: &'static str,
        message: impl Into<String>,
        owner: BodyOwner,
        loc: InferLoc,
    ) -> Self {
        LowerError::Error {
            code,
            message: message.into(),
            at: Some((owner, loc)),
        }
    }

    /// Construct not lowered yet (E1301).
    pub(crate) fn unsupported(what: impl fmt::Display, owner: BodyOwner, loc: InferLoc) -> Self {
        Self::at("E1301", format!("{what} is not lowered yet"), owner, loc)
    }

    /// Places the error at `loc` if it has no place yet.
    pub(crate) fn located(self, owner: BodyOwner, loc: InferLoc) -> Self {
        match self {
            LowerError::Error {
                code,
                message,
                at: None,
            } => LowerError::Error {
                code,
                message,
                at: Some((owner, loc)),
            },
            other => other,
        }
    }

    /// As a source diagnostic (with the range of what it is about, when it has a place).
    pub fn to_diagnostic(
        &self,
        cx: &Cx<'_>,
    ) -> Result<Option<marlin_lang_diag::Diagnostic>, Cancelled> {
        match self {
            LowerError::Cancelled(_) => Ok(None),
            LowerError::Error {
                code,
                message,
                at: Some((owner, loc)),
            } => {
                let d = InferDiagnostic {
                    owner: *owner,
                    loc: loc.clone(),
                    code,
                    message: message.clone(),
                };
                d.to_diagnostic(cx).map(Some)
            }
            LowerError::Error { code, message, .. } => Ok(Some(
                marlin_lang_diag::Diagnostic::error(marlin_lang_diag::Code(code), message.clone()),
            )),
        }
    }
}

impl fmt::Display for LowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LowerError::Cancelled(_) => f.write_str("cancelled"),
            LowerError::Error { code, message, .. } => write!(f, "error[{code}]: {message}"),
        }
    }
}

impl std::error::Error for LowerError {}
