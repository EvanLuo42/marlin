//! hashbrown maps with the engine's fast hasher.

use marlin_hash::FastState;

pub use hashbrown::{HashTable, hash_map, hash_set, hash_table};

/// `hashbrown::HashMap` hashed with [`FastState`].
pub type HashMap<K, V, S = FastState> = hashbrown::HashMap<K, V, S>;

/// `hashbrown::HashSet` hashed with [`FastState`].
pub type HashSet<T, S = FastState> = hashbrown::HashSet<T, S>;

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
