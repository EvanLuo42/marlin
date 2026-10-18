//! The IR interpreter (0006 §11, 0005 §4.6): runs functions for compile-time evaluation and
//! whole compute dispatches for differential testing. Its results are the IR's semantics.
//!
//! - All lanes of a thread group step through the structured control flow together: a
//!   divergent branch runs the lanes that took each side in turn (true side first), and they
//!   reconverge at the merge block; loops iterate while any lane stays in them. Wave
//!   operations work per wave (consecutive lanes, `Options::wave_size`) over that wave's active
//!   lanes, so barriers are always met by the whole group at once. Groups run one after
//!   another.
//! - Instructions run lane by lane in ascending order, so atomics and racing stores give the
//!   same result every time.
//! - Floating-point operations compute in `f64` and round once to the result type; the
//!   transcendental functions come from [`crate::math`], the same on every host (0006 §11).
//! - Integer division by zero gives 0 (0006 §10); out-of-range memory accesses read zero and
//!   drop writes, and are counted in [`Output::out_of_bounds`].
//! - Resources are buffers the caller provides; a `res<K>` value is an index into them.
//!   Textures, ray queries, mesh output and pixel operations are not interpreted yet
//!   (0005 §11 step 10).

mod eval;
mod exec;
pub(crate) mod memory;

use std::fmt;

use crate::func::FuncId;
use crate::module::{GlobalId, Module};
use crate::num::f16_to_f32;
use crate::ty::{Scalar, Type, TypeData};

/// A value: scalar bits (integers zero-extended, floats as IEEE bits, resource indices), the
/// parts of a vector (components), matrix (columns), array or struct, a pointer or a handle.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Val {
    Bits(u64),
    Agg(Vec<Val>),
    Ptr(Ptr),
    /// A materialized descriptor: the buffer's index.
    Handle(u32),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ptr {
    /// Into a value tree: a variable or module variable, then indices into its parts.
    Logical { root: Root, path: Vec<u64> },
    /// Into a buffer, at a byte offset (`u64::MAX` once it is out of range).
    Bytes { buffer: u32, offset: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Root {
    /// A `function` variable.
    Var(u32),
    /// A module-level variable; `lane` is the lane for per-lane spaces (input, output), else 0.
    Global(GlobalId, u32),
}

impl Val {
    pub fn i32(v: i32) -> Val {
        Val::Bits(v as u32 as u64)
    }

    pub fn u32(v: u32) -> Val {
        Val::Bits(v as u64)
    }

    pub fn f32(v: f32) -> Val {
        Val::Bits(v.to_bits() as u64)
    }

    pub fn f64(v: f64) -> Val {
        Val::Bits(v.to_bits())
    }

    pub fn bool(v: bool) -> Val {
        Val::Bits(v as u64)
    }

    /// The all-zero value of type `t` (what `zero` constants and fresh variables hold).
    pub fn zero(m: &Module, t: Type) -> Result<Val, ExecError> {
        memory::zero(m, t)
    }

    pub fn bits(&self) -> Option<u64> {
        match self {
            Val::Bits(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_i32(&self) -> Option<i32> {
        self.bits().map(|b| b as u32 as i32)
    }

    pub fn as_f32(&self) -> Option<f32> {
        self.bits().map(|b| f32::from_bits(b as u32))
    }

    pub fn parts(&self) -> Option<&[Val]> {
        match self {
            Val::Agg(items) => Some(items),
            _ => None,
        }
    }

    /// The value as the text format writes constants of type `t` (for debug output).
    pub fn show(&self, m: &Module, t: Type) -> String {
        match (self, m.data(t)) {
            (Val::Bits(b), TypeData::Scalar(s)) => show_scalar(*b, *s),
            (Val::Bits(b), TypeData::Res(_)) => b.to_string(),
            (Val::Agg(items), TypeData::Vector(s, _)) => {
                let parts: Vec<String> = items
                    .iter()
                    .map(|i| i.bits().map_or("?".into(), |b| show_scalar(b, *s)))
                    .collect();
                format!("({})", parts.join(", "))
            }
            (Val::Agg(items), _) => {
                let parts: Vec<String> = items.iter().map(|i| format!("{i:?}")).collect();
                format!("({})", parts.join(", "))
            }
            (v, _) => format!("{v:?}"),
        }
    }
}

fn show_scalar(b: u64, s: Scalar) -> String {
    match s {
        Scalar::I1 => (b != 0).to_string(),
        Scalar::I16 => (b as u16 as i16).to_string(),
        Scalar::I32 => (b as u32 as i32).to_string(),
        Scalar::I64 => (b as i64).to_string(),
        Scalar::F16 => format!("{:?}", f16_to_f32(b as u16)),
        Scalar::F32 => format!("{:?}", f32::from_bits(b as u32)),
        Scalar::F64 => format!("{:?}", f64::from_bits(b)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecError {
    pub func: Option<Box<str>>,
    /// The instruction, as it prints.
    pub at: Option<String>,
    pub message: String,
}

impl ExecError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            func: None,
            at: None,
            message: message.into(),
        }
    }
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(func) = &self.func {
            write!(f, "@{func}: ")?;
        }
        if let Some(at) = &self.at {
            write!(f, "`{at}`: ")?;
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for ExecError {}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Lanes per wave: a power of two from 1 to 128.
    pub wave_size: u32,
    /// The most instructions a run may execute (each counted once, whatever the lanes), so a
    /// loop that never ends is an error instead of a hang.
    pub step_limit: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            wave_size: 32,
            step_limit: 50_000_000,
        }
    }
}

/// What a dispatch reads and writes.
#[derive(Clone, Debug, Default)]
pub struct Resources {
    /// The buffers, by resource index; dispatches write into them.
    pub buffers: Vec<Vec<u8>>,
    /// The value of the entry point's `push` variable, if it has one.
    pub push: Option<Val>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Print {
    pub group: [u32; 3],
    /// The lane's index in its group.
    pub lane: u32,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    /// `debug.print`, in execution order.
    pub prints: Vec<Print>,
    /// `debug.assert`s that failed, with their messages.
    pub failed_asserts: Vec<Print>,
    pub out_of_bounds: u64,
    /// Instructions executed.
    pub steps: u64,
}

pub struct Interpreter<'m> {
    pub module: &'m Module,
    pub options: Options,
}

impl<'m> Interpreter<'m> {
    pub fn new(module: &'m Module) -> Self {
        Self {
            module,
            options: Options::default(),
        }
    }

    /// Calls `func` on one lane with `args`: compile-time evaluation.
    pub fn call(&self, func: FuncId, args: &[Val]) -> Result<Vec<Val>, ExecError> {
        let mut resources = Resources::default();
        let mut out = Output::default();
        exec::call(self, func, args, &mut resources, &mut out)
    }

    /// Runs compute entry point `entry` for `groups` thread groups.
    pub fn dispatch(
        &self,
        entry: FuncId,
        groups: [u32; 3],
        resources: &mut Resources,
    ) -> Result<Output, ExecError> {
        let mut out = Output::default();
        exec::dispatch(self, entry, groups, resources, &mut out)?;
        Ok(out)
    }
}

/// Evaluates a side-effect-free operation on known values, exactly as the interpreter would:
/// what constant folding uses, so folded and executed code agree.
pub fn evaluate_op(
    m: &Module,
    op: &crate::op::Op,
    args: &[Val],
    arg_types: &[Type],
    result: Option<Type>,
) -> Result<Val, ExecError> {
    eval::eval(m, op, args, arg_types, result)
}
