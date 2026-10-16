//! Lazily initialized thread-local values that are dropped when their thread exits.

use alloc::boxed::Box;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::fmt;
use core::mem::MaybeUninit;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use marlin_pal::futex;
use marlin_pal::tls::Key;

/// A thread-local value of type `T`, created on each thread's first [`with`](Self::with) and
/// dropped when that thread exits.
///
/// Built on an OS TLS key, so each access is a call into the OS (`FlsGetValue`,
/// `pthread_getspecific`). For hot data without a destructor prefer a `#[thread_local]` static.
/// Destructors are not guaranteed to run for the main thread or at process exit.
///
/// ```ignore
/// static SCRATCH: LocalKey<RefCell<Vec<u8>>> = LocalKey::new(|| RefCell::new(Vec::new()));
/// SCRATCH.with(|s| s.borrow_mut().push(1));
/// ```
pub struct LocalKey<T: 'static> {
    /// `KEY_*`; the OS key is created on first use because statics cannot call the OS.
    state: AtomicU32,
    key: UnsafeCell<MaybeUninit<Key>>,
    init: fn() -> T,
}

// SAFETY: the only shared mutable state is `key`, written once before the Release store of
// `KEY_READY` and read only after an Acquire load observes it. Values never leave their thread.
unsafe impl<T: 'static> Sync for LocalKey<T> {}

const KEY_UNINIT: u32 = 0;
const KEY_CREATING: u32 = 1;
const KEY_READY: u32 = 2;

/// Stored in the slot while a thread's value is being dropped, so access from its destructor is
/// caught rather than creating a value nobody would free.
const DESTROYED: *mut c_void = ptr::without_provenance_mut(1);

/// What a thread's slot points to. Holds the key so the exit destructor can mark the slot.
struct Slot<T> {
    key: Key,
    value: T,
}

impl<T: 'static> LocalKey<T> {
    pub const fn new(init: fn() -> T) -> Self {
        Self {
            state: AtomicU32::new(KEY_UNINIT),
            key: UnsafeCell::new(MaybeUninit::uninit()),
            init,
        }
    }

    /// Calls `f` with this thread's value, creating it first if needed.
    ///
    /// # Panics
    /// If the OS runs out of TLS keys or memory for the slot, if the initializer accesses this key,
    /// or if called from the value's own destructor.
    pub fn with<R>(&'static self, f: impl FnOnce(&T) -> R) -> R {
        let key = self.key();
        // SAFETY: keys of a `LocalKey` are never deleted.
        let mut slot = unsafe { key.get() };
        if slot.is_null() {
            slot = self.initialize(key);
        } else if slot == DESTROYED {
            panic!("LocalKey accessed while its value is being destroyed");
        }
        // SAFETY: a non-null, non-sentinel slot is this thread's `Box<Slot<T>>` from `initialize`.
        // Only the exit destructor frees it, which marks the slot first and cannot run during
        // `with` except from the value's own drop, caught above.
        f(unsafe { &(*slot.cast::<Slot<T>>()).value })
    }

    #[inline]
    fn key(&self) -> Key {
        if self.state.load(Ordering::Acquire) == KEY_READY {
            // SAFETY: `KEY_READY` is stored (Release) only after `key` is written.
            return unsafe { (*self.key.get()).assume_init() };
        }
        self.create_key()
    }

    #[cold]
    fn create_key(&self) -> Key {
        loop {
            match self.state.compare_exchange(
                KEY_UNINIT,
                KEY_CREATING,
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    let key = match Key::new(Some(destroy::<T>)) {
                        Ok(key) => key,
                        Err(error) => {
                            self.state.store(KEY_UNINIT, Ordering::Release);
                            futex::wake_all(&self.state);
                            panic!("failed to create a TLS key: {error}");
                        }
                    };
                    // SAFETY: winning the CAS makes us the only writer, and readers wait for
                    // `KEY_READY`.
                    unsafe { (*self.key.get()).write(key) };
                    self.state.store(KEY_READY, Ordering::Release);
                    futex::wake_all(&self.state);
                    return key;
                }
                Err(KEY_READY) => {
                    // SAFETY: observed `KEY_READY` with Acquire, so `key` is written.
                    return unsafe { (*self.key.get()).assume_init() };
                }
                Err(_) => {
                    futex::wait(&self.state, KEY_CREATING, None);
                }
            }
        }
    }

    #[cold]
    fn initialize(&self, key: Key) -> *mut c_void {
        let value = (self.init)();
        // SAFETY: keys of a `LocalKey` are never deleted.
        if !unsafe { key.get() }.is_null() {
            panic!("LocalKey initializer accessed its own key");
        }
        let slot = Box::into_raw(Box::new(Slot { key, value })).cast::<c_void>();
        // SAFETY: as above.
        if let Err(error) = unsafe { key.set(slot) } {
            // SAFETY: `slot` was never published, so we still own it.
            drop(unsafe { Box::from_raw(slot.cast::<Slot<T>>()) });
            panic!("failed to set a TLS value: {error}");
        }
        slot
    }
}

impl<T: 'static> fmt::Debug for LocalKey<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalKey").finish_non_exhaustive()
    }
}

/// Exit destructor of every `LocalKey<T>`.
///
/// # Safety
///
/// `value` must be `DESTROYED` or a `Box<Slot<T>>` pointer from `LocalKey::<T>::initialize`.
unsafe extern "C" fn destroy<T>(value: *mut c_void) {
    // POSIX calls destructors again for values set during destruction, i.e. our sentinel.
    if value == DESTROYED {
        return;
    }
    // SAFETY: per the contract this is the thread's `Box<Slot<T>>`, and the OS hands it to us
    // exactly once.
    let slot = unsafe { Box::from_raw(value.cast::<Slot<T>>()) };
    // SAFETY: keys of a `LocalKey` are never deleted. If marking fails, access from `T::drop`
    // re-creates a value that leaks; nothing unsound.
    let _ = unsafe { slot.key.set(DESTROYED) };
    drop(slot);
}
