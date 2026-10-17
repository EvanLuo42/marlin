//! Synchronization primitives built on wait-on-address:
//!
//! - locks: [`Mutex`] (and [`RawMutex`]), [`RwLock`] (and [`RawRwLock`]), each with timed
//!   `try_*_for` variants; [`ReentrantMutex`]; [`SpinLock`] for sections too short to sleep in
//! - waiting: [`Condvar`], [`Event`], [`Parker`], [`Semaphore`], [`Barrier`]
//! - one-time initialization: [`Once`], [`OnceLock`], [`LazyLock`]
//! - message passing: [`Channel`], a bounded MPMC queue in a fixed array
//!
//! Every blocking primitive is a few atomics plus [`marlin_pal::futex`]: no allocation, no OS
//! handles, and every constructor is `const`, so all of them can live in a `static`.
//!
//! There is no poisoning: the engine runs with `panic = "abort"`, so a lock can never be observed
//! after its holder panicked. (Tests unwind; a guard dropped during unwinding simply unlocks.)
//!
//! Guards are `Send` when the data allows it: a futex lock has no owning thread, so it may be
//! released from a different thread than the one that acquired it (e.g. a fiber that migrated).
//! The exception is [`ReentrantMutex`], which is owned by an OS thread and is not fiber-safe.
#![cfg_attr(not(test), no_std)]

mod barrier;
mod channel;
mod condvar;
mod event;
mod mutex;
mod once;
mod parker;
mod reentrant;
mod rwlock;
mod semaphore;
mod spin;
mod timeout;

pub use barrier::{Barrier, BarrierWaitResult};
pub use channel::{
    Channel, RecvError, RecvTimeoutError, SendError, SendTimeoutError, TryRecvError, TrySendError,
};
pub use condvar::{Condvar, WaitTimeoutResult};
pub use event::{Event, ResetMode};
pub use mutex::{Mutex, MutexGuard, RawMutex};
pub use once::{LazyLock, Once, OnceLock};
pub use parker::Parker;
pub use reentrant::{ReentrantMutex, ReentrantMutexGuard};
pub use rwlock::{RawRwLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
pub use semaphore::{Semaphore, SemaphorePermit};
pub use spin::{SpinLock, SpinLockGuard};

/// Spins this many times on a held lock before sleeping. Long enough to ride out a short critical
/// section on another core, short enough not to burn a time slice.
const SPIN_LIMIT: u32 = 100;
