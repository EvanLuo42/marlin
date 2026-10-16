//! Closure-based spawning on top of the PAL's allocation-free [`ThreadStart`] entry.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::fmt;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};

use marlin_pal::thread::ThreadStart;
use marlin_pal::{CpuSet, Error, ErrorKind, ThreadPriority, futex, process};

/// Configures a thread before spawning it.
///
/// Name, affinity and priority are applied by the new thread itself, before the closure runs.
/// When affinity or priority is set, [`spawn`](Self::spawn) waits until the thread has applied
/// them so it can report failure; a thread whose setup failed never runs the closure.
#[derive(Clone, Default)]
#[must_use = "a Builder does nothing until spawned"]
pub struct Builder {
    name: Option<String>,
    stack_size: usize,
    affinity: Option<CpuSet>,
    priority: Option<ThreadPriority>,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Names the thread for debuggers and profilers. Best effort; truncated to the platform limit
    /// (15 bytes on Linux).
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Stack size in bytes; 0 (the default) is the platform default. Rounded up to the platform
    /// minimum and page size.
    pub fn stack_size(mut self, bytes: usize) -> Self {
        self.stack_size = bytes;
        self
    }

    /// Restricts the thread to `cpus` (ids from [`cpu_topology`](crate::cpu_topology)).
    ///
    /// Ignored where the OS has no hard affinity (macOS), so portable code can always set it;
    /// steer work there with [`priority`](Self::priority). On Windows the set must lie in one
    /// processor group.
    pub fn affinity(mut self, cpus: CpuSet) -> Self {
        self.affinity = Some(cpus);
        self
    }

    /// See [`ThreadPriority`] for the per-platform mapping. Raising priority above `Normal` on
    /// Linux needs `CAP_SYS_NICE` or a sufficient `RLIMIT_NICE`, else spawning fails with
    /// `PermissionDenied`.
    pub fn priority(mut self, priority: ThreadPriority) -> Self {
        self.priority = Some(priority);
        self
    }

    /// Starts a thread running `f`.
    ///
    /// Fails if the OS cannot create the thread, or the thread cannot apply its affinity or
    /// priority.
    pub fn spawn<F, T>(self, f: F) -> Result<JoinHandle<T>, Error>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let shared = Arc::new(Shared {
            setup: AtomicU32::new(SETUP_PENDING),
            setup_error: UnsafeCell::new(None),
            result: UnsafeCell::new(None),
        });
        let must_wait = self.affinity.is_some() || self.priority.is_some();
        let stack_size = self.stack_size;
        let packet = Box::new(Packet {
            start: ThreadStart {
                entry: entry::<F, T>,
            },
            config: self,
            shared: shared.clone(),
            main: f,
        });
        let start = NonNull::from(Box::leak(packet)).cast::<ThreadStart>();

        // SAFETY: `start` points to a leaked `Packet<F, T>` whose first field is a `ThreadStart`
        // (`repr(C)`); `entry::<F, T>` takes ownership of it. It is reclaimed below if the
        // thread never starts.
        let native = match unsafe { marlin_pal::thread::spawn(stack_size, start) } {
            Ok(native) => native,
            Err(error) => {
                // SAFETY: the thread was not created, so we still own the packet.
                drop(unsafe { Box::from_raw(start.cast::<Packet<F, T>>().as_ptr()) });
                return Err(error);
            }
        };

        if must_wait {
            let mut state = shared.setup.load(Ordering::Acquire);
            while state == SETUP_PENDING {
                futex::wait(&shared.setup, SETUP_PENDING, None);
                state = shared.setup.load(Ordering::Acquire);
            }
            if state == SETUP_FAILED {
                // The thread is exiting without running the closure; reap it.
                let _ = native.join();
                // SAFETY: the thread wrote the error before its Release store of
                // `SETUP_FAILED`, which our Acquire load observed, and never touches it again.
                let error = unsafe { *shared.setup_error.get() };
                return Err(error.unwrap_or(Error::from_kind(ErrorKind::Other)));
            }
        }

        Ok(JoinHandle { native, shared })
    }
}

impl fmt::Debug for Builder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Builder")
            .field("name", &self.name)
            .field("stack_size", &self.stack_size)
            .field("affinity", &self.affinity)
            .field("priority", &self.priority)
            .finish()
    }
}

/// An owned thread whose closure returns `T`. Dropping it detaches the thread.
#[must_use = "dropping a JoinHandle detaches the thread"]
pub struct JoinHandle<T> {
    native: marlin_pal::thread::JoinHandle,
    shared: Arc<Shared<T>>,
}

impl<T> JoinHandle<T> {
    /// Waits for the thread to finish and returns what its closure returned.
    ///
    /// There is no error case for panics: a panic aborts the process.
    ///
    /// # Panics
    /// If the OS fails to join the thread.
    pub fn join(mut self) -> T {
        if let Err(error) = self.native.join() {
            panic!("failed to join thread: {error}");
        }
        // The thread dropped its reference before exiting; `get_mut`'s Acquire on the count
        // pairs with that Release, which makes the result write visible.
        let shared = Arc::get_mut(&mut self.shared).expect("joined thread still holds its packet");
        shared
            .result
            .get_mut()
            .take()
            .expect("joined thread left no result")
    }

    /// Lets the thread run on its own; its result is dropped when it finishes.
    pub fn detach(self) {}
}

impl<T> fmt::Debug for JoinHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinHandle").finish_non_exhaustive()
    }
}

const SETUP_PENDING: u32 = 0;
const SETUP_DONE: u32 = 1;
const SETUP_FAILED: u32 = 2;

/// State shared by the new thread and its [`JoinHandle`].
struct Shared<T> {
    /// `SETUP_*`; waited on with the futex when the spawner needs the setup outcome.
    setup: AtomicU32,
    /// Written once by the new thread before it publishes `SETUP_FAILED`.
    setup_error: UnsafeCell<Option<Error>>,
    /// Written once by the new thread, read by `join` after the thread released its `Arc`.
    result: UnsafeCell<Option<T>>,
}

// SAFETY: each cell has a single writer (the new thread) and is only read after a
// happens-before edge with that write (the `setup` Release store, or the `Arc` count drop), so
// sharing only ever moves a `T` between threads.
unsafe impl<T: Send> Sync for Shared<T> {}

/// The new thread's start data. `start` must stay first so the PAL's pointer to it is also a
/// pointer to the whole packet.
#[repr(C)]
struct Packet<F, T> {
    start: ThreadStart,
    config: Builder,
    shared: Arc<Shared<T>>,
    main: F,
}

/// Aborts if a panic unwinds out of the thread's closure (only possible in tests, which unwind),
/// so it never crosses the OS entry point and `join` never sees a missing result.
struct AbortOnUnwind;

impl Drop for AbortOnUnwind {
    fn drop(&mut self) {
        process::abort();
    }
}

/// # Safety
///
/// `start` must come from [`Builder::spawn`] for the same `F` and `T`, and be passed exactly once.
unsafe fn entry<F, T>(start: NonNull<ThreadStart>)
where
    F: FnOnce() -> T,
{
    // SAFETY: per the contract `start` is the leaked `Packet<F, T>` and we are its only owner.
    let packet = unsafe { Box::from_raw(start.cast::<Packet<F, T>>().as_ptr()) };
    let Packet {
        start: _,
        config,
        shared,
        main,
    } = *packet;
    let guard = AbortOnUnwind;

    if let Some(name) = &config.name {
        marlin_pal::thread::set_current_name(name);
    }
    let setup = apply(&config);
    if config.affinity.is_some() || config.priority.is_some() {
        let state = match setup {
            Ok(()) => SETUP_DONE,
            Err(error) => {
                // SAFETY: only this thread writes the cell, and the spawner reads it only after
                // the Release store below.
                unsafe { *shared.setup_error.get() = Some(error) };
                SETUP_FAILED
            }
        };
        shared.setup.store(state, Ordering::Release);
        futex::wake_one(&shared.setup);
        if state == SETUP_FAILED {
            drop(main);
            core::mem::forget(guard);
            return;
        }
    }

    let result = main();
    // SAFETY: only this thread writes the cell; `join` reads it after we drop `shared`.
    unsafe { *shared.result.get() = Some(result) };
    drop(shared);
    core::mem::forget(guard);
}

fn apply(config: &Builder) -> Result<(), Error> {
    if let Some(cpus) = &config.affinity {
        match marlin_pal::thread::set_current_affinity(cpus) {
            Err(error) if error.kind() != ErrorKind::Unsupported => return Err(error),
            _ => {}
        }
    }
    if let Some(priority) = config.priority {
        marlin_pal::thread::set_current_priority(priority)?;
    }
    Ok(())
}
