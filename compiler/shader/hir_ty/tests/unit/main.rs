//! marlin-shaderc-hir-ty's tests (quick tier, docs/design/0014-testing.md §5): built-ins of a
//! stand-in core library, type lowering of signatures, and type diagnostics (golden files).
//! Platform-independent.
//! Run alone with `cargo test -p marlin-shaderc-hir-ty --test unit`; `MARLIN_BLESS=1` rewrites
//! the golden files.

mod support;

mod types;
