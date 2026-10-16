//! CPU-architecture code, orthogonal to the OS: fiber context switching per calling convention.
//!
//! Every variant follows one scheme. `switch` pushes the callee-saved state onto the current
//! stack, stores the stack pointer in `*save`, loads the target's stack pointer, pops its state and
//! returns into it. A fresh context is a hand-built frame whose return address is a trampoline,
//! with the entry function and its argument in callee-saved registers. Backends pick the variant
//! for their ABI; console backends may reuse these.

#[cfg(target_arch = "aarch64")]
pub(crate) mod aarch64;
#[cfg(target_arch = "x86_64")]
pub(crate) mod x86_64;
