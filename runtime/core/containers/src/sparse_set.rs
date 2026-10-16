//! `SparseSet<T>`: `u32` keys mapped to densely packed values.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

/// Keys per sparse page. Pages are allocated on first use, so a few large keys (entity indices
/// late in a long session) cost 4 KiB each instead of a sparse array up to the largest key.
const PAGE_LEN: usize = 1024;
const ABSENT: u32 = u32::MAX;

/// Maps `u32` keys to values stored contiguously: lookup is two array reads, iteration is a slice
/// walk, and removal swaps the last value into the hole (so order is not preserved).
///
/// The usual ECS building block: keys are entity indices, values are components.
pub struct SparseSet<T> {
    /// key -> dense index, paged; `ABSENT` where there is no value.
    sparse: Vec<Option<Box<[u32; PAGE_LEN]>>>,
    /// dense index -> key.
    keys: Vec<u32>,
    values: Vec<T>,
}

impl<T> SparseSet<T> {
    pub const fn new() -> Self {
        Self {
            sparse: Vec::new(),
            keys: Vec::new(),
            values: Vec::new(),
        }
    }

    /// Room for `capacity` values without reallocating the dense arrays.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            sparse: Vec::new(),
            keys: Vec::with_capacity(capacity),
            values: Vec::with_capacity(capacity),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Where `key`'s value sits in [`values`](Self::values).
    #[inline]
    pub fn dense_index(&self, key: u32) -> Option<usize> {
        let page = self.sparse.get(key as usize / PAGE_LEN)?.as_ref()?;
        match page[key as usize % PAGE_LEN] {
            ABSENT => None,
            i => Some(i as usize),
        }
    }

    #[inline]
    pub fn contains(&self, key: u32) -> bool {
        self.dense_index(key).is_some()
    }

    #[inline]
    pub fn get(&self, key: u32) -> Option<&T> {
        self.dense_index(key).map(|i| &self.values[i])
    }

    #[inline]
    pub fn get_mut(&mut self, key: u32) -> Option<&mut T> {
        self.dense_index(key).map(|i| &mut self.values[i])
    }

    /// Inserts or replaces; returns the previous value.
    pub fn insert(&mut self, key: u32, value: T) -> Option<T> {
        if let Some(i) = self.dense_index(key) {
            return Some(core::mem::replace(&mut self.values[i], value));
        }
        // Dense indices must stay below ABSENT.
        assert!(self.values.len() < ABSENT as usize, "SparseSet is full");
        *self.sparse_slot(key) = self.values.len() as u32;
        self.keys.push(key);
        self.values.push(value);
        None
    }

    /// Removes and returns `key`'s value. The last value moves into its place.
    pub fn remove(&mut self, key: u32) -> Option<T> {
        let i = self.dense_index(key)?;
        *self.sparse_slot(key) = ABSENT;
        let last = self.keys.len() - 1;
        if i != last {
            let moved = self.keys[last];
            *self.sparse_slot(moved) = i as u32;
        }
        self.keys.swap_remove(i);
        Some(self.values.swap_remove(i))
    }

    /// Removes every value; keeps the allocated pages.
    pub fn clear(&mut self) {
        for &key in &self.keys {
            if let Some(Some(page)) = self.sparse.get_mut(key as usize / PAGE_LEN) {
                page[key as usize % PAGE_LEN] = ABSENT;
            }
        }
        self.keys.clear();
        self.values.clear();
    }

    /// Keys in dense order (parallel to [`values`](Self::values)).
    #[inline]
    pub fn keys(&self) -> &[u32] {
        &self.keys
    }

    #[inline]
    pub fn values(&self) -> &[T] {
        &self.values
    }

    #[inline]
    pub fn values_mut(&mut self) -> &mut [T] {
        &mut self.values
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (u32, &T)> + DoubleEndedIterator + '_ {
        self.keys.iter().copied().zip(self.values.iter())
    }

    pub fn iter_mut(
        &mut self,
    ) -> impl ExactSizeIterator<Item = (u32, &mut T)> + DoubleEndedIterator + '_ {
        self.keys.iter().copied().zip(self.values.iter_mut())
    }

    /// Swaps two values' dense positions (for sorting or grouping without moving keys).
    pub fn swap_dense(&mut self, a: usize, b: usize) {
        if a == b {
            return;
        }
        let (ka, kb) = (self.keys[a], self.keys[b]);
        self.keys.swap(a, b);
        self.values.swap(a, b);
        *self.sparse_slot(ka) = b as u32;
        *self.sparse_slot(kb) = a as u32;
    }

    fn sparse_slot(&mut self, key: u32) -> &mut u32 {
        let page = key as usize / PAGE_LEN;
        if page >= self.sparse.len() {
            self.sparse.resize_with(page + 1, || None);
        }
        let page = self.sparse[page].get_or_insert_with(|| Box::new([ABSENT; PAGE_LEN]));
        &mut page[key as usize % PAGE_LEN]
    }
}

impl<T> Default for SparseSet<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone> Clone for SparseSet<T> {
    fn clone(&self) -> Self {
        Self {
            sparse: self.sparse.clone(),
            keys: self.keys.clone(),
            values: self.values.clone(),
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for SparseSet<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}
