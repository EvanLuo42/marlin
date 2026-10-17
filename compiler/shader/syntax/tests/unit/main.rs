//! marlin-shaderc-syntax's tests (quick tier, docs/design/0014-testing.md §5): the shader dialect
//! accepts the syntax of 0004 and rejects the C# constructs it leaves out, with hints.
//! Platform-independent.
//! Run alone with `cargo test -p marlin-shaderc-syntax --test unit`.

mod dialect;
