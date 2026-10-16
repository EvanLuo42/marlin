//! Platform abstraction layer: the thin, unsafe OS interface beneath every Marlin wrapper crate.
//!
//! Only L0 (`runtime/base`) and L1 (`runtime/platform`) crates may depend on this crate; everything
//! above goes through their safe wrappers (`cargo xtask check-layers` enforces this). The PAL does
//! not allocate: it is what the allocator is built on.
//!
//! # Structure
//!
//! - [`contract`](crate::contract) — traits every backend's `Backend` type must implement.
//! - Public modules ([`vmem`], [`process`], ...) — `#[inline]` free functions forwarding to
//!   `<imp::Backend as Trait>`.
//! - [`ext`] — capability extensions. Compile time (`cfg(marlin_ext = "...")`, from the platform
//!   manifest) says whether the platform has the API; a run-time `get()` says whether this
//!   process can use it.
//! - `backend` — exactly one backend, chosen by `cfg(marlin_platform = "...")`. Console backends
//!   are pulled in from `platforms/<name>/pal/`.
#![cfg_attr(not(test), no_std)]

mod arch;
mod backend;
mod contract;
pub mod debug;
mod error;
pub mod ext;
pub mod fiber;
pub mod futex;
pub mod process;
pub mod sysinfo;
pub mod thread;
pub mod time;
pub mod tls;
mod types;
pub mod vmem;

pub use error::{Error, ErrorKind, Result};
pub use types::{CpuSet, Protection, ThreadPriority};

// The selected backend must implement the whole contract, even items nothing calls yet.
const _: () = {
    const fn assert_platform<P: contract::Platform>() {}
    assert_platform::<backend::imp::Backend>();
};
