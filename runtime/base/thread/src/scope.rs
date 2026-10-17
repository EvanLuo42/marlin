//! Scoped threads, which may borrow from the stack of the thread that opened the scope.
//!
//! The design is `std::thread::scope`'s: [`scope`] counts the threads spawned in it and does not
//! return until the count is back to zero, so a thread's closure can borrow anything that
//! outlives the scope. The lifetimes are invariant so neither can be shrunk or stretched to let a
//! borrow escape.

use alloc::sync::Arc;
use core::fmt;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicU32, Ordering};

use marlin_pal::futex;

use crate::{Builder, JoinHandle};

/// Opens a scope for spawning threads that borrow non-`'static` data, then waits for all of them.
///
/// `f` gets a [`Scope`] to spawn on. Every thread spawned in it, including threads spawned by
/// scoped threads, has finished (and dropped its closure and unjoined result) when `scope`
/// returns. Threads not joined through their [`ScopedJoinHandle`] are joined here.
///
/// A panic in a scoped thread aborts the process, like any thread here, so `scope` never returns
/// after one. If `f` itself unwinds (only in tests), the unwind waits for the threads first.
///
/// ```ignore
/// let mut counts = [0u32; 4];
/// let name = String::from("pool");
/// thread::scope(|s| {
///     for slot in &mut counts {
///         s.spawn(|| *slot = name.len() as u32);
///     }
/// });
/// ```
pub fn scope<'env, F, T>(f: F) -> T
where
    F: for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> T,
{
    let scope = Scope {
        data: Arc::new(ScopeData {
            running: AtomicU32::new(0),
        }),
        scope: PhantomData,
        env: PhantomData,
    };
    // Waits on drop, so an unwinding `f` cannot free borrowed frames under a running thread.
    let wait = WaitOnDrop(&scope.data);
    let result = f(&scope);
    drop(wait);
    result
}

/// A scope to spawn threads in; see [`scope`].
///
/// `'scope` is the scope's own lifetime and `'env` that of the data its threads may borrow. Both
/// are invariant.
pub struct Scope<'scope, 'env: 'scope> {
    data: Arc<ScopeData>,
    scope: PhantomData<&'scope mut &'scope ()>,
    env: PhantomData<&'env mut &'env ()>,
}

impl<'scope, 'env> Scope<'scope, 'env> {
    /// Starts a thread running `f`, which may borrow anything that outlives the scope.
    ///
    /// The thread is joined when the scope ends unless [`ScopedJoinHandle::join`] joins it first.
    /// Use [`Builder::spawn_scoped`] for name, stack size, affinity or priority.
    ///
    /// # Panics
    /// If the OS cannot create the thread.
    pub fn spawn<F, T>(&'scope self, f: F) -> ScopedJoinHandle<'scope, T>
    where
        F: FnOnce() -> T + Send + 'scope,
        T: Send + 'scope,
    {
        match Builder::new().spawn_scoped(self, f) {
            Ok(handle) => handle,
            Err(error) => panic!("failed to spawn scoped thread: {error}"),
        }
    }

    pub(crate) fn data(&self) -> &Arc<ScopeData> {
        &self.data
    }
}

impl fmt::Debug for Scope<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scope")
            .field("running", &self.data.running.load(Ordering::Relaxed))
            .finish()
    }
}

/// A thread spawned in a [`Scope`]. Dropping it leaves the thread to be joined by the scope.
pub struct ScopedJoinHandle<'scope, T> {
    inner: JoinHandle<T>,
    scope: PhantomData<&'scope ()>,
}

impl<'scope, T> ScopedJoinHandle<'scope, T> {
    pub(crate) fn new(inner: JoinHandle<T>) -> Self {
        Self {
            inner,
            scope: PhantomData,
        }
    }

    /// Waits for the thread to finish and returns what its closure returned.
    ///
    /// There is no error case for panics: a panic aborts the process.
    ///
    /// # Panics
    /// If the OS fails to join the thread.
    pub fn join(self) -> T {
        self.inner.join()
    }
}

impl<T> fmt::Debug for ScopedJoinHandle<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScopedJoinHandle").finish_non_exhaustive()
    }
}

/// Shared by a scope and its threads. Each thread holds an `Arc`, so the counter outlives the
/// wake that follows its last decrement even if the scope has already returned.
pub(crate) struct ScopeData {
    /// Threads spawned and not yet done with borrowed data. Waited on with the futex.
    running: AtomicU32,
}

impl ScopeData {
    /// Counts a thread about to be spawned.
    ///
    /// # Panics
    /// If an absurd number of threads is running, before the counter can wrap.
    pub(crate) fn increment(&self) {
        if self.running.fetch_add(1, Ordering::Relaxed) > u32::MAX / 2 {
            self.decrement();
            panic!("too many running threads in thread scope");
        }
    }

    /// Uncounts a thread; it must touch nothing borrowed afterwards. Release so the scope's
    /// Acquire wait sees everything the thread wrote.
    pub(crate) fn decrement(&self) {
        if self.running.fetch_sub(1, Ordering::Release) == 1 {
            futex::wake_one(&self.running);
        }
    }

    fn wait(&self) {
        loop {
            let running = self.running.load(Ordering::Acquire);
            if running == 0 {
                return;
            }
            futex::wait(&self.running, running, None);
        }
    }
}

struct WaitOnDrop<'a>(&'a ScopeData);

impl Drop for WaitOnDrop<'_> {
    fn drop(&mut self) {
        self.0.wait();
    }
}
