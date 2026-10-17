//! Types for the Marlin shader language (`docs/design/0005-shader-compiler.md` §2, step 5 of
//! §11; the rules are in `0004-shader-language.md`).
//!
//! Step 5a, here so far:
//! - [`Ty`]: semantic types. Built-in types are declarations of the core library marked
//!   `[Builtin("name")]` (0004 §17.1); the [`Builtins`] registry says which is which, and the
//!   scalar keywords (`float`, `int`, ...) name them.
//! - Type lowering: written types → [`Ty`], with aliases expanded, type arguments checked
//!   against parameters (constant parameters take constants: `Vector<T, N * 2>`), and constant
//!   arithmetic folded or kept symbolic.
//! - [`ItemTypes`]: a declaration's signature as types: generic parameters and their interface
//!   bounds, parameters, the result or field type, implemented interfaces, `[Builtin]` and
//!   `[Intrinsic]`.
//! - [`FileTyDiagnostics`]: every diagnostic of a file up to here.

mod builtins;
mod diagnostics;
mod display;
mod item;
mod lower;
mod ty;

use marlin_front_hir_def::Language;
use marlin_shaderc_syntax::SHADER;

pub use builtins::{BuiltinKind, Builtins, BuiltinsQuery, Resource, Scalar};
pub use diagnostics::{FileTyDiagnostics, TyDiagnostic, TyDiagnosticKind, TyLoc};
pub use display::{display_const, display_ty};
pub use item::{AliasTargetQuery, GenericParamTy, ItemTypes, ItemTypesQuery, ParamTy, self_ty};
pub use ty::{ConstExpr, ConstOp, ConstTy, GenericArgTy, Ty};

/// The Marlin shader language, for `hir_def`'s `LanguageInput`.
pub static SHADER_LANGUAGE: Language = Language {
    name: "Marlin shader",
    dialect: &SHADER,
    implicit_usings: &["Marlin"],
    require_namespace: true,
};
