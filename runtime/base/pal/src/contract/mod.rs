//! The platform contract.
//!
//! Every backend exposes a `Backend` type implementing [`Platform`]. The public API forwards to
//! `<imp::Backend as Trait>`, so a backend with a missing item or a wrong signature fails to compile
//! on its own platform, not at some call site higher up.

mod debug;
pub(crate) mod ext;
mod fiber;
mod file;
mod fs;
mod futex;
mod process;
mod sysinfo;
mod thread;
mod time;
mod tls;
mod vmem;
mod watch;

pub(crate) use debug::Debug;
pub(crate) use fiber::Fiber;
pub(crate) use file::File;
pub(crate) use fs::Fs;
pub(crate) use futex::Futex;
pub(crate) use process::Process;
pub(crate) use sysinfo::SysInfo;
pub(crate) use thread::Thread;
pub(crate) use time::Time;
pub(crate) use tls::Tls;
pub(crate) use vmem::VMem;
pub(crate) use watch::Watch;

/// The whole contract. Grows with each subsystem.
pub(crate) trait Platform:
    VMem + Process + Debug + Time + Thread + Futex + Tls + SysInfo + Fiber + File + Fs + Watch
{
}
