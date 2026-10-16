use core::cell::{Cell, UnsafeCell};
use core::fmt;
use core::mem::MaybeUninit;
use core::ops::Deref;
use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering::{Acquire, Relaxed, Release};

use marlin_pal::futex;

const INCOMPLETE: u32 = 0;
const RUNNING: u32 = 1;
/// Running, and other threads are asleep waiting for it.
const QUEUED: u32 = 2;
const COMPLETE: u32 = 3;

/// Runs a one-time initialization. Threads that arrive while it runs block until it finishes.
///
/// If the closure unwinds (tests only; the engine aborts on panic) the `Once` returns to
/// incomplete and the next caller runs its closure instead.
pub struct Once {
    state: AtomicU32,
}

impl Once {
    #[inline]
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(INCOMPLETE),
        }
    }

    /// Whether some `call_once` has finished. If `true`, its effects are visible to this thread.
    #[inline]
    pub fn is_completed(&self) -> bool {
        self.state.load(Acquire) == COMPLETE
    }

    /// Runs `f` if no call has completed yet; returns once one has. Calling this re-entrantly
    /// from inside `f` deadlocks.
    #[inline]
    pub fn call_once(&self, f: impl FnOnce()) {
        if !self.is_completed() {
            self.call_once_slow(&mut Some(f));
        }
    }

    #[cold]
    fn call_once_slow<F: FnOnce()>(&self, f: &mut Option<F>) {
        let mut state = self.state.load(Acquire);
        loop {
            match state {
                COMPLETE => return,
                INCOMPLETE => {
                    if let Err(s) = self
                        .state
                        .compare_exchange_weak(INCOMPLETE, RUNNING, Acquire, Acquire)
                    {
                        state = s;
                        continue;
                    }
                    // Publishes the result (or resets the state if `f` unwinds) and wakes waiters.
                    let mut finish = Finish {
                        state: &self.state,
                        set_to: INCOMPLETE,
                    };
                    (f.take().unwrap())();
                    finish.set_to = COMPLETE;
                    return;
                }
                RUNNING => {
                    if let Err(s) = self
                        .state
                        .compare_exchange_weak(RUNNING, QUEUED, Relaxed, Acquire)
                    {
                        state = s;
                        continue;
                    }
                    state = QUEUED;
                }
                _ => {
                    debug_assert_eq!(state, QUEUED);
                    futex::wait(&self.state, QUEUED, None);
                    state = self.state.load(Acquire);
                }
            }
        }
    }
}

struct Finish<'a> {
    state: &'a AtomicU32,
    set_to: u32,
}

impl Drop for Finish<'_> {
    fn drop(&mut self) {
        if self.state.swap(self.set_to, Release) == QUEUED {
            futex::wake_all(self.state);
        }
    }
}

impl Default for Once {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Once {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Once")
            .field("completed", &self.is_completed())
            .finish()
    }
}

/// A cell written at most once, shareable between threads.
pub struct OnceLock<T> {
    once: Once,
    value: UnsafeCell<MaybeUninit<T>>,
}

// SAFETY: `&OnceLock` hands out `&T` to every thread (needs `Sync`), and the thread that
// initializes it may not be the one that drops it (needs `Send`).
unsafe impl<T: Send + Sync> Sync for OnceLock<T> {}
// SAFETY: moving the cell moves the `T`.
unsafe impl<T: Send> Send for OnceLock<T> {}

impl<T> OnceLock<T> {
    #[inline]
    pub const fn new() -> Self {
        Self {
            once: Once::new(),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    #[inline]
    pub fn get(&self) -> Option<&T> {
        if self.once.is_completed() {
            // SAFETY: completion means the value was written, and the Acquire load in
            // `is_completed` makes the write visible. It is never written again.
            Some(unsafe { (*self.value.get()).assume_init_ref() })
        } else {
            None
        }
    }

    #[inline]
    pub fn get_mut(&mut self) -> Option<&mut T> {
        if self.once.is_completed() {
            // SAFETY: initialized (see `get`); `&mut self` makes this the only access.
            Some(unsafe { self.value.get_mut().assume_init_mut() })
        } else {
            None
        }
    }

    /// Stores `value` unless the cell is already set, in which case `value` comes back.
    pub fn set(&self, value: T) -> Result<(), T> {
        let mut value = Some(value);
        self.get_or_init(|| value.take().unwrap());
        match value {
            None => Ok(()),
            Some(value) => Err(value),
        }
    }

    /// Returns the value, running `f` to create it first if the cell is empty. If several threads
    /// race, exactly one `f` runs and the rest wait for it.
    #[inline]
    pub fn get_or_init(&self, f: impl FnOnce() -> T) -> &T {
        if let Some(value) = self.get() {
            return value;
        }
        self.once.call_once(|| {
            // SAFETY: the `Once` grants exclusive access while it runs, and nobody reads the
            // value before it completes.
            unsafe { (*self.value.get()).write(f()) };
        });
        // SAFETY: `call_once` returns only after a call completed, which wrote the value.
        unsafe { (*self.value.get()).assume_init_ref() }
    }

    #[inline]
    pub fn into_inner(mut self) -> Option<T> {
        self.take()
    }

    /// Empties the cell, returning the value if there was one.
    pub fn take(&mut self) -> Option<T> {
        if self.once.is_completed() {
            self.once = Once::new();
            // SAFETY: the value was initialized, and resetting `once` first means it is treated as
            // uninitialized from now on, so it is read out exactly once.
            Some(unsafe { self.value.get_mut().assume_init_read() })
        } else {
            None
        }
    }
}

impl<T> Drop for OnceLock<T> {
    fn drop(&mut self) {
        if self.once.is_completed() {
            // SAFETY: initialized, and never accessed again.
            unsafe { self.value.get_mut().assume_init_drop() };
        }
    }
}

impl<T> Default for OnceLock<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> From<T> for OnceLock<T> {
    fn from(value: T) -> Self {
        let cell = Self::new();
        let _ = cell.set(value);
        cell
    }
}

impl<T: fmt::Debug> fmt::Debug for OnceLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_tuple("OnceLock");
        match self.get() {
            Some(value) => d.field(value),
            None => d.field(&format_args!("<uninit>")),
        };
        d.finish()
    }
}

/// A value initialized on first access, typically in a `static`.
pub struct LazyLock<T, F = fn() -> T> {
    cell: OnceLock<T>,
    // Taken by whichever thread runs the initialization, under the `Once`.
    init: Cell<Option<F>>,
}

// SAFETY: `init` is only touched inside the `Once`, i.e. by one thread at a time, and may run on
// any thread (needs `F: Send`). The value is shared as in `OnceLock`.
unsafe impl<T: Send + Sync, F: Send> Sync for LazyLock<T, F> {}

impl<T, F: FnOnce() -> T> LazyLock<T, F> {
    #[inline]
    pub const fn new(init: F) -> Self {
        Self {
            cell: OnceLock::new(),
            init: Cell::new(Some(init)),
        }
    }

    /// Forces initialization and returns the value. Same as dereferencing.
    #[inline]
    pub fn force(this: &Self) -> &T {
        this.cell.get_or_init(|| match this.init.take() {
            Some(init) => init(),
            // Only reachable if an earlier initializer unwound (tests only).
            None => panic!("LazyLock initializer panicked earlier"),
        })
    }
}

impl<T, F: FnOnce() -> T> Deref for LazyLock<T, F> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        Self::force(self)
    }
}

impl<T: Default> Default for LazyLock<T> {
    fn default() -> Self {
        Self::new(T::default)
    }
}

impl<T: fmt::Debug, F> fmt::Debug for LazyLock<T, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_tuple("LazyLock");
        match self.cell.get() {
            Some(value) => d.field(value),
            None => d.field(&format_args!("<uninit>")),
        };
        d.finish()
    }
}
