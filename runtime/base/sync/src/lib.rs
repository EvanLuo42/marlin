//! Mutex, RwLock, Condvar, Once and Event built on wait-on-address.
//!
//! Every blocking primitive is a few atomics plus [`marlin_pal::futex`]: no allocation, no OS
//! handles, and every constructor is `const`, so all of them can live in a `static`.
//!
//! There is no poisoning: the engine runs with `panic = "abort"`, so a lock can never be observed
//! after its holder panicked. (Tests unwind; a guard dropped during unwinding simply unlocks.)
//!
//! Guards are `Send` when the data allows it: a futex lock has no owning thread, so it may be
//! released from a different thread than the one that acquired it (e.g. a fiber that migrated).
#![cfg_attr(not(test), no_std)]

mod condvar;
mod event;
mod mutex;
mod once;
mod parker;
mod rwlock;
mod timeout;

pub use condvar::{Condvar, WaitTimeoutResult};
pub use event::{Event, ResetMode};
pub use mutex::{Mutex, MutexGuard, RawMutex};
pub use once::{LazyLock, Once, OnceLock};
pub use parker::Parker;
pub use rwlock::{RawRwLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Spins this many times on a held lock before sleeping. Long enough to ride out a short critical
/// section on another core, short enough not to burn a time slice.
const SPIN_LIMIT: u32 = 100;
