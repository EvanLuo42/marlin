//! Types for the Marlin shader language (`docs/design/0005-shader-compiler.md` §2, step 5 of
//! §11; the rules are in `0004-shader-language.md`).
//!
//! Step 5a:
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
//!
//! Step 5b: [`InferQuery`] types each body C#-style and bidirectionally: literals take their type
//! from context, implicit conversions follow 0004 §9.3–9.4, operators and calls go through
//! C#'s overload rules (§9.8) with generic and constant-parameter inference, members include
//! swizzles, extension blocks and interface members of bounds. The result records what MIR
//! lowering needs: types, value paths, callees and implicit conversions.
//!
//! Step 5c, the remaining rules, which check without changing types:
//! - [`BodyChecksQuery`]: C#'s definite assignment, `out` parameters and returns on every
//!   path, no fall-through between `switch` sections, writes to and non-`readonly` calls on
//!   read-only values, exhaustive `switch`.
//! - [`ItemChecksQuery`]: where interfaces, `Func`/`Action` and `bool` may appear, interface
//!   implementations, namespace-level `[GroupShared]` variables, entry points and system values,
//!   push constants, `Uniform` rows, `[Requires]` contracts.
//! - Their tools: compiler-known attributes ([`ItemAttributesQuery`]), scalar layout
//!   ([`layout_of`]) and capability inference ([`CapabilitiesQuery`]).

mod attrs;
mod builtins;
mod caps;
mod check;
mod diagnostics;
mod display;
mod infer;
mod item;
mod layout;
mod lower;
mod subst;
mod ty;

use marlin_front_hir_def::Language;
use marlin_shaderc_syntax::SHADER;

pub use attrs::{
    AttributeTy, ItemAttributes, ItemAttributesQuery, KnownAttr, Stage, SystemValue,
    known_attribute,
};
pub use builtins::{BuiltinKind, Builtins, BuiltinsQuery, Resource, Scalar};
pub use caps::{Capabilities, CapabilitiesQuery, declared_requirements};
pub use check::{BodyChecksQuery, ItemChecksQuery};
pub use diagnostics::{FileTyDiagnostics, TyDiagnostic, TyDiagnosticKind, TyLoc};
pub use display::{display_const, display_ty};
pub use infer::{
    Callee, InferDiagnostic, InferLoc, InferQuery, InferenceResult, Root, Step, ValuePath,
};
pub use item::{AliasTargetQuery, GenericParamTy, ItemTypes, ItemTypesQuery, ParamTy, self_ty};
pub use layout::{FieldLayout, Layout, fields_of, layout_of};
pub use lower::parse_int;
pub use subst::Subst;
pub use ty::{ConstExpr, ConstOp, ConstTy, GenericArgTy, Ty};

/// The Marlin shader language, for `hir_def`'s `LanguageInput`.
pub static SHADER_LANGUAGE: Language = Language {
    name: "Marlin shader",
    dialect: &SHADER,
    implicit_usings: &["Marlin"],
    require_namespace: true,
};
