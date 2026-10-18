//! The Marlin shader IR (`docs/design/0006-shader-ir.md`, step 6 of `0005-shader-compiler.md`
//! §11): one instruction set from lowering to the backends.
//!
//! Step 6a:
//! - [`Module`]: interned types ([`TypeData`]) and constants, structs, module-level variables
//!   (entry I/O, groupshared, constant data), functions and entry points.
//! - [`Function`]: blocks, instructions and values in arenas, block parameters instead of phis,
//!   structured terminators, instructions in doubly linked lists, a use list per value.
//! - [`Op`]: the closed operation set, with immediates in the op and every value read as an
//!   operand.
//! - [`FuncBuilder`]: building functions with inferred result types.
//! - The text format: [`print_module`] and [`parse_module`], which round-trip.
//!
//! Step 6b: [`verify_module`] checks types and constants, module variables and entry points
//! (bindings, stages, `uses`), the call graph (no runtime recursion), and each function: blocks
//! and terminators, use lists, operand and result types, jump arguments, dominance, where
//! pointers and handles may go, and structured control flow.
//!
//! Step 6c: the [`Interpreter`] runs functions (compile-time evaluation) and compute dispatches
//! (differential testing), with the whole thread group in lockstep over structured control
//! flow, waves, groupshared memory, barriers and atomics, buffers in scalar layout, and the
//! deterministic [`math`] functions.
//!
//! Step 8a, for the passes: [`mod@cfg`] (jumps, dominators and structured regions, shared with the
//! verifier); removing instructions, parameters and blocks' contents; [`constant_value`] and
//! [`value_constant`] between constants and interpreter values.
//!
//! Step 8c: the verifier lets a break leave only the innermost loop or switch, as SPIR-V does;
//! `assume.uniform` and `assume.nonuniform`; source locations for instructions
//! ([`SourceLoc`]), which the text format leaves out.

macro_rules! words {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $word:literal),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),*
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),*];

            pub fn name(self) -> &'static str {
                match self {
                    $($name::$variant => $word),*
                }
            }

            pub fn from_name(word: &str) -> Option<$name> {
                Self::ALL.iter().copied().find(|v| v.name() == word)
            }
        }
    };
}

mod builder;
pub mod cfg;
mod func;
mod infer;
mod interp;
pub mod math;
mod module;
pub mod num;
mod op;
mod parse;
mod print;
mod ty;
mod verify;

pub use builder::FuncBuilder;
pub use func::{
    Block, BlockData, FuncId, Function, Inst, InstData, InstIter, Use, Value, ValueData, ValueDef,
};
pub use infer::{Existing, Types, result_types, result_types_in, step_type, step_type_in};
pub use interp::{
    ExecError, Interpreter, Options, Output, Print, Ptr, Resources, Root, Val, constant_value,
    evaluate_op, scalar_field_offset, scalar_layout, value_constant,
};
pub use module::{
    Binding, Builtin, Const, ConstData, ConstValue, Entry, GlobalData, GlobalId, Interp, Module,
    SourceLoc, Stage, StructData,
};
pub use op::{
    AccessStep, AtomicOp, Axis, BinaryOp, ConvertOp, FloatCmp, IntCmp, MathOp, MemOrder, Op,
    QuadOp, ReduceOp, Scope, SwitchCase, TexArg, UnaryOp, VoteOp, WaveOp,
};
pub use parse::{ParseError, parse_module};
pub use print::{FuncText, block_order, const_lit, print_function, print_module, type_name};
pub use ty::{ResKind, ResourceKind, Scalar, Space, StructId, TextureShape, Type, TypeData};
pub use verify::{VerifyError, early_mesh_writes, verify_module};
