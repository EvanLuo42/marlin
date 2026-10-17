//! hashbrown maps with the engine's fast hasher.

use alloc::alloc::{Allocator, Global};

use marlin_hash::FastState;

pub use hashbrown::{HashTable, hash_map, hash_set, hash_table};

/// `hashbrown::HashMap` hashed with [`FastState`], allocating from `A`.
pub type HashMap<K, V, S = FastState, A = Global> = hashbrown::HashMap<K, V, S, A>;

/// `hashbrown::HashSet` hashed with [`FastState`], allocating from `A`.
pub type HashSet<T, S = FastState, A = Global> = hashbrown::HashSet<T, S, A>;

/// `new` and `with_capacity` for [`HashMap`]: hashbrown only provides them for its own default
/// hasher, which the engine does not use.
pub trait HashMapExt {
    fn new() -> Self;
    fn with_capacity(capacity: usize) -> Self;
}

impl<K, V> HashMapExt for HashMap<K, V> {
    #[inline]
    fn new() -> Self {
        Self::with_hasher(FastState::new())
    }

    #[inline]
    fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity_and_hasher(capacity, FastState::new())
    }
}

/// `new_in` and `with_capacity_in` for a [`HashMap`] in another allocator, with [`FastState`].
/// A separate trait from [`HashMapExt`] so that `HashMap::new()` still infers the global
/// allocator.
pub trait HashMapInExt<A> {
    fn new_in(alloc: A) -> Self;
    fn with_capacity_in(capacity: usize, alloc: A) -> Self;
}

impl<K, V, A: Allocator> HashMapInExt<A> for HashMap<K, V, FastState, A> {
    #[inline]
    fn new_in(alloc: A) -> Self {
        Self::with_hasher_in(FastState::new(), alloc)
    }

    #[inline]
    fn with_capacity_in(capacity: usize, alloc: A) -> Self {
        Self::with_capacity_and_hasher_in(capacity, FastState::new(), alloc)
    }
}

/// `new` and `with_capacity` for [`HashSet`]; see [`HashMapExt`].
pub trait HashSetExt {
    fn new() -> Self;
    fn with_capacity(capacity: usize) -> Self;
}

impl<T> HashSetExt for HashSet<T> {
    #[inline]
    fn new() -> Self {
        Self::with_hasher(FastState::new())
    }

    #[inline]
    fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity_and_hasher(capacity, FastState::new())
    }
}

/// `new_in` and `with_capacity_in` for [`HashSet`]; see [`HashMapInExt`].
pub trait HashSetInExt<A> {
    fn new_in(alloc: A) -> Self;
    fn with_capacity_in(capacity: usize, alloc: A) -> Self;
}

impl<T, A: Allocator> HashSetInExt<A> for HashSet<T, FastState, A> {
    #[inline]
    fn new_in(alloc: A) -> Self {
        Self::with_hasher_in(FastState::new(), alloc)
    }

    #[inline]
    fn with_capacity_in(capacity: usize, alloc: A) -> Self {
        Self::with_capacity_and_hasher_in(capacity, FastState::new(), alloc)
    }
}
