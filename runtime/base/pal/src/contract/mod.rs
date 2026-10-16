//! The platform contract.
//!
//! Every backend exposes a `Backend` type implementing [`Platform`]. The public API forwards to
//! `<imp::Backend as Trait>`, so a backend with a missing item or a wrong signature fails to compile
//! on its own platform, not at some call site higher up.

pub(crate) mod ext;
mod process;
mod vmem;

pub(crate) use process::Process;
pub(crate) use vmem::VMem;

/// The whole contract. Grows with each subsystem (thread, fiber, futex, time, file, ...).
pub(crate) trait Platform: VMem + Process {}
