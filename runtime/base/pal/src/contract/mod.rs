//! The platform contract.
//!
//! Every backend exposes a `Backend` type implementing [`Platform`]. The public API forwards to
//! `<imp::Backend as Trait>`, so a backend with a missing item or a wrong signature fails to compile
//! on its own platform, not at some call site higher up.

mod debug;
pub(crate) mod ext;
mod futex;
mod process;
mod thread;
mod time;
mod tls;
mod vmem;

pub(crate) use debug::Debug;
pub(crate) use futex::Futex;
pub(crate) use process::Process;
pub(crate) use thread::Thread;
pub(crate) use time::Time;
pub(crate) use tls::Tls;
pub(crate) use vmem::VMem;

/// The whole contract. Grows with each subsystem (fiber, file, ...).
pub(crate) trait Platform: VMem + Process + Debug + Time + Thread + Futex + Tls {}
