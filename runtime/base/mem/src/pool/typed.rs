//! A pool of one type.

use core::alloc::Layout;
use core::fmt;
use core::marker::PhantomData;
use core::ptr::NonNull;

use super::Pool;

/// A [`Pool`] of `T`s: values are moved in by [`alloc`](Self::alloc) and dropped by
/// [`free`](Self::free) (or moved out by [`take`](Self::take)). Values still in the pool when it
/// is dropped are not dropped.
pub struct TypedPool<T> {
    pool: Pool,
    _values: PhantomData<T>,
}

// SAFETY: the pool is `Send + Sync`; values move in and out of it, and are dropped, on whichever
// thread calls, so sharing the pool across threads requires `T: Send` (as `Mutex<T>` does).
unsafe impl<T: Send> Send for TypedPool<T> {}
// SAFETY: as above.
unsafe impl<T: Send> Sync for TypedPool<T> {}

impl<T> TypedPool<T> {
    /// Room for at least `capacity` values.
    pub fn new(capacity: usize) -> marlin_pal::Result<Self> {
        Ok(Self {
            pool: Pool::new(Layout::new::<T>(), capacity)?,
            _values: PhantomData,
        })
    }

    /// Moves `value` into the pool. Gives it back once the pool is full.
    #[inline]
    pub fn alloc(&self, value: T) -> Result<NonNull<T>, T> {
        match self.pool.alloc() {
            Some(p) => {
                let p = p.cast::<T>();
                // SAFETY: a fresh block, sized and aligned for a `T`.
                unsafe { p.write(value) };
                Ok(p)
            }
            None => Err(value),
        }
    }

    /// Drops the value at `ptr` and returns its block to the pool.
    ///
    /// # Safety
    /// `ptr` came from this pool's [`alloc`](Self::alloc), is not used afterwards and is freed
    /// once.
    #[inline]
    pub unsafe fn free(&self, ptr: NonNull<T>) {
        // SAFETY: a live value of this pool (the caller).
        unsafe {
            ptr.drop_in_place();
            self.pool.free(ptr.cast());
        }
    }

    /// Moves the value at `ptr` out and returns its block to the pool.
    ///
    /// # Safety
    /// As for [`free`](Self::free).
    #[inline]
    pub unsafe fn take(&self, ptr: NonNull<T>) -> T {
        // SAFETY: a live value of this pool (the caller), read once and then forgotten.
        unsafe {
            let value = ptr.read();
            self.pool.free(ptr.cast());
            value
        }
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.pool.capacity()
    }

    /// Values in the pool.
    #[inline]
    pub fn len(&self) -> usize {
        self.pool.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.pool.is_empty()
    }

    #[inline]
    pub fn committed(&self) -> usize {
        self.pool.committed()
    }

    /// See [`Pool::trim`].
    pub fn trim(&mut self) {
        self.pool.trim();
    }

    /// The untyped pool underneath.
    #[inline]
    pub fn pool(&self) -> &Pool {
        &self.pool
    }
}

impl<T> fmt::Debug for TypedPool<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("TypedPool").field(&self.pool).finish()
    }
}
