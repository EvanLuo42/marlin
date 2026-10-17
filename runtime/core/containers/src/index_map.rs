//! `IndexMap` / `IndexSet`: hash maps that remember insertion order.

use alloc::alloc::{Allocator, Global};
use alloc::vec::Vec;
use core::borrow::Borrow;
use core::cmp::Ordering;
use core::hash::{BuildHasher, Hash};
use core::iter::FusedIterator;
use core::ops::{Index, IndexMut};
use core::{fmt, mem, slice};

use hashbrown::HashTable;
use marlin_hash::FastState;

use crate::SmallVec;
use crate::small_vec;

struct Bucket<K, V> {
    /// Kept so the table can rehash, and entries move, without hashing keys again.
    hash: u64,
    key: K,
    value: V,
}

impl<K: Clone, V: Clone> Clone for Bucket<K, V> {
    fn clone(&self) -> Self {
        Self {
            hash: self.hash,
            key: self.key.clone(),
            value: self.value.clone(),
        }
    }
}

/// A hash map whose entries stay in insertion order, addressable by position as well as by key.
///
/// The entries live in a `Vec` (so iteration is a slice walk and `get_index` is O(1)); a
/// [`HashTable`] maps hashes to their positions. Removal comes in two flavors:
/// [`swap_remove`](Self::swap_remove) moves the last entry into the hole (O(1), perturbs the
/// order), [`shift_remove`](Self::shift_remove) shifts the later entries down (O(n), keeps it).
/// Re-inserting an existing key keeps its position.
///
/// Both arrays allocate from `A`. Equality ignores order, as for other maps.
pub struct IndexMap<K, V, S = FastState, A: Allocator = Global> {
    entries: Vec<Bucket<K, V>, A>,
    indices: HashTable<usize, A>,
    hasher: S,
}

impl<K, V> IndexMap<K, V> {
    pub const fn new() -> Self {
        Self::with_hasher(FastState::new())
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity_and_hasher(capacity, FastState::new())
    }
}

impl<K, V, S> IndexMap<K, V, S> {
    pub const fn with_hasher(hasher: S) -> Self {
        Self {
            entries: Vec::new(),
            indices: HashTable::new(),
            hasher,
        }
    }

    pub fn with_capacity_and_hasher(capacity: usize, hasher: S) -> Self {
        Self::with_capacity_and_hasher_in(capacity, hasher, Global)
    }
}

impl<K, V, A: Allocator + Clone> IndexMap<K, V, FastState, A> {
    pub fn new_in(alloc: A) -> Self {
        Self::with_capacity_and_hasher_in(0, FastState::new(), alloc)
    }
}

impl<K, V, S, A: Allocator + Clone> IndexMap<K, V, S, A> {
    pub fn with_hasher_in(hasher: S, alloc: A) -> Self {
        Self::with_capacity_and_hasher_in(0, hasher, alloc)
    }

    pub fn with_capacity_and_hasher_in(capacity: usize, hasher: S, alloc: A) -> Self {
        Self {
            entries: Vec::with_capacity_in(capacity, alloc.clone()),
            indices: HashTable::with_capacity_in(capacity, alloc),
            hasher,
        }
    }
}

impl<K, V, S, A: Allocator> IndexMap<K, V, S, A> {
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries that fit without reallocating.
    pub fn capacity(&self) -> usize {
        self.entries.capacity().min(self.indices.capacity())
    }

    #[inline]
    pub fn hasher(&self) -> &S {
        &self.hasher
    }

    #[inline]
    pub fn allocator(&self) -> &A {
        self.entries.allocator()
    }

    /// Makes room for `additional` more entries.
    pub fn reserve(&mut self, additional: usize) {
        let entries = &self.entries;
        self.indices.reserve(additional, |&i| entries[i].hash);
        self.entries.reserve(additional);
    }

    pub fn clear(&mut self) {
        self.indices.clear();
        self.entries.clear();
    }

    /// The entry at `index`, in insertion order.
    #[inline]
    pub fn get_index(&self, index: usize) -> Option<(&K, &V)> {
        self.entries.get(index).map(|b| (&b.key, &b.value))
    }

    /// The entry at `index`, with the value mutable.
    #[inline]
    pub fn get_index_mut(&mut self, index: usize) -> Option<(&K, &mut V)> {
        self.entries.get_mut(index).map(|b| (&b.key, &mut b.value))
    }

    #[inline]
    pub fn first(&self) -> Option<(&K, &V)> {
        self.get_index(0)
    }

    #[inline]
    pub fn last(&self) -> Option<(&K, &V)> {
        self.entries.last().map(|b| (&b.key, &b.value))
    }

    /// Removes and returns the last entry.
    pub fn pop(&mut self) -> Option<(K, V)> {
        let b = self.entries.pop()?;
        let index = self.entries.len();
        self.erase(b.hash, index);
        Some((b.key, b.value))
    }

    /// Removes the entry at `index`, moving the last entry into its place. O(1).
    pub fn swap_remove_index(&mut self, index: usize) -> Option<(K, V)> {
        let hash = self.entries.get(index)?.hash;
        self.erase(hash, index);
        let last = self.entries.len() - 1;
        if index != last {
            let moved = self.entries[last].hash;
            self.relocate(moved, last, index);
        }
        let b = self.entries.swap_remove(index);
        Some((b.key, b.value))
    }

    /// Removes the entry at `index`, shifting the later entries down. O(n); keeps the order.
    pub fn shift_remove_index(&mut self, index: usize) -> Option<(K, V)> {
        let hash = self.entries.get(index)?.hash;
        self.erase(hash, index);
        let moved = self.entries.len() - index - 1;
        if moved > self.indices.len() / 2 {
            for i in self.indices.iter_mut() {
                if *i > index {
                    *i -= 1;
                }
            }
        } else {
            for i in index + 1..self.entries.len() {
                let h = self.entries[i].hash;
                self.relocate(h, i, i - 1);
            }
        }
        let b = self.entries.remove(index);
        Some((b.key, b.value))
    }

    /// Keeps the entries for which `f` returns true, in order.
    pub fn retain(&mut self, mut f: impl FnMut(&K, &mut V) -> bool) {
        let before = self.entries.len();
        self.entries.retain_mut(|b| f(&b.key, &mut b.value));
        if self.entries.len() != before {
            self.rebuild();
        }
    }

    /// Sorts the entries with `cmp`, which sees each key and value. Not stable, but keys are
    /// unique, so ties can only come from a comparison that ignores them.
    pub fn sort_unstable_by(&mut self, mut cmp: impl FnMut(&K, &V, &K, &V) -> Ordering) {
        self.entries
            .sort_unstable_by(|a, b| cmp(&a.key, &a.value, &b.key, &b.value));
        self.rebuild();
    }

    /// Sorts the entries by key.
    pub fn sort_keys(&mut self)
    where
        K: Ord,
    {
        self.entries.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        self.rebuild();
    }

    /// Reverses the order of the entries.
    pub fn reverse(&mut self) {
        self.entries.reverse();
        let n = self.entries.len();
        for i in self.indices.iter_mut() {
            *i = n - 1 - *i;
        }
    }

    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter {
            inner: self.entries.iter(),
        }
    }

    pub fn iter_mut(&mut self) -> IterMut<'_, K, V> {
        IterMut {
            inner: self.entries.iter_mut(),
        }
    }

    pub fn keys(&self) -> impl ExactSizeIterator<Item = &K> + DoubleEndedIterator + '_ {
        self.entries.iter().map(|b| &b.key)
    }

    pub fn values(&self) -> impl ExactSizeIterator<Item = &V> + DoubleEndedIterator + '_ {
        self.entries.iter().map(|b| &b.value)
    }

    pub fn values_mut(
        &mut self,
    ) -> impl ExactSizeIterator<Item = &mut V> + DoubleEndedIterator + '_ {
        self.entries.iter_mut().map(|b| &mut b.value)
    }

    /// Removes the table's record of entry `index`.
    fn erase(&mut self, hash: u64, index: usize) {
        match self.indices.find_entry(hash, |&i| i == index) {
            Ok(e) => {
                e.remove();
            }
            Err(_) => unreachable!("IndexMap table out of sync"),
        }
    }

    /// Points the table's record of entry `from` at `to`.
    fn relocate(&mut self, hash: u64, from: usize, to: usize) {
        match self.indices.find_mut(hash, |&i| i == from) {
            Some(i) => *i = to,
            None => unreachable!("IndexMap table out of sync"),
        }
    }

    /// Rebuilds the table after the entries were reordered.
    fn rebuild(&mut self) {
        self.indices.clear();
        let entries = &self.entries;
        for (i, b) in entries.iter().enumerate() {
            self.indices.insert_unique(b.hash, i, |&j| entries[j].hash);
        }
    }
}

impl<K: Hash + Eq, V, S: BuildHasher, A: Allocator> IndexMap<K, V, S, A> {
    /// Inserts or replaces; returns the previous value. A replaced entry keeps its position.
    #[inline]
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.insert_full(key, value).1
    }

    /// [`insert`](Self::insert), also returning the entry's position.
    pub fn insert_full(&mut self, key: K, value: V) -> (usize, Option<V>) {
        let hash = self.hasher.hash_one(&key);
        let entries = &self.entries;
        if let Some(&i) = self.indices.find(hash, |&i| entries[i].key == key) {
            return (i, Some(mem::replace(&mut self.entries[i].value, value)));
        }
        let i = self.push(hash, key, value);
        (i, None)
    }

    /// The value for `key`, inserting `f()` at the end if there is none.
    pub fn get_or_insert_with(&mut self, key: K, f: impl FnOnce() -> V) -> &mut V {
        let hash = self.hasher.hash_one(&key);
        let entries = &self.entries;
        let i = match self.indices.find(hash, |&i| entries[i].key == key) {
            Some(&i) => i,
            None => self.push(hash, key, f()),
        };
        &mut self.entries[i].value
    }

    fn push(&mut self, hash: u64, key: K, value: V) -> usize {
        let i = self.entries.len();
        let entries = &self.entries;
        self.indices.insert_unique(hash, i, |&j| entries[j].hash);
        self.entries.push(Bucket { hash, key, value });
        i
    }

    /// The position of `key`'s entry.
    pub fn get_index_of<Q: Hash + Eq + ?Sized>(&self, key: &Q) -> Option<usize>
    where
        K: Borrow<Q>,
    {
        if self.entries.is_empty() {
            return None;
        }
        let hash = self.hasher.hash_one(key);
        self.indices
            .find(hash, |&i| self.entries[i].key.borrow() == key)
            .copied()
    }

    #[inline]
    pub fn contains_key<Q: Hash + Eq + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.get_index_of(key).is_some()
    }

    #[inline]
    pub fn get<Q: Hash + Eq + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.get_index_of(key).map(|i| &self.entries[i].value)
    }

    #[inline]
    pub fn get_mut<Q: Hash + Eq + ?Sized>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        self.get_index_of(key).map(|i| &mut self.entries[i].value)
    }

    /// Position, key and value of `key`'s entry.
    pub fn get_full<Q: Hash + Eq + ?Sized>(&self, key: &Q) -> Option<(usize, &K, &V)>
    where
        K: Borrow<Q>,
    {
        let i = self.get_index_of(key)?;
        let b = &self.entries[i];
        Some((i, &b.key, &b.value))
    }

    /// Removes `key`'s entry, moving the last entry into its place. O(1).
    #[inline]
    pub fn swap_remove<Q: Hash + Eq + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        self.swap_remove_full(key).map(|(_, _, v)| v)
    }

    /// [`swap_remove`](Self::swap_remove), also returning the old position and the key.
    pub fn swap_remove_full<Q: Hash + Eq + ?Sized>(&mut self, key: &Q) -> Option<(usize, K, V)>
    where
        K: Borrow<Q>,
    {
        let i = self.get_index_of(key)?;
        let (k, v) = self.swap_remove_index(i)?;
        Some((i, k, v))
    }

    /// Removes `key`'s entry, shifting the later entries down. O(n); keeps the order.
    #[inline]
    pub fn shift_remove<Q: Hash + Eq + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        self.shift_remove_full(key).map(|(_, _, v)| v)
    }

    /// [`shift_remove`](Self::shift_remove), also returning the old position and the key.
    pub fn shift_remove_full<Q: Hash + Eq + ?Sized>(&mut self, key: &Q) -> Option<(usize, K, V)>
    where
        K: Borrow<Q>,
    {
        let i = self.get_index_of(key)?;
        let (k, v) = self.shift_remove_index(i)?;
        Some((i, k, v))
    }
}

impl<K, V, S: Default> Default for IndexMap<K, V, S> {
    fn default() -> Self {
        Self::with_hasher(S::default())
    }
}

impl<K: Clone, V: Clone, S: Clone, A: Allocator + Clone> Clone for IndexMap<K, V, S, A> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            indices: self.indices.clone(),
            hasher: self.hasher.clone(),
        }
    }
}

impl<K: fmt::Debug, V: fmt::Debug, S, A: Allocator> fmt::Debug for IndexMap<K, V, S, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K, V, S1, S2, A1, A2> PartialEq<IndexMap<K, V, S2, A2>> for IndexMap<K, V, S1, A1>
where
    K: Hash + Eq,
    V: PartialEq,
    S1: BuildHasher,
    S2: BuildHasher,
    A1: Allocator,
    A2: Allocator,
{
    /// The same entries, in any order.
    fn eq(&self, other: &IndexMap<K, V, S2, A2>) -> bool {
        self.len() == other.len() && self.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}

impl<K: Hash + Eq, V: Eq, S: BuildHasher, A: Allocator> Eq for IndexMap<K, V, S, A> {}

impl<K, Q, V, S, A> Index<&Q> for IndexMap<K, V, S, A>
where
    K: Hash + Eq + Borrow<Q>,
    Q: Hash + Eq + ?Sized,
    S: BuildHasher,
    A: Allocator,
{
    type Output = V;

    /// Panics if the key is absent.
    #[track_caller]
    fn index(&self, key: &Q) -> &V {
        self.get(key).expect("key not in IndexMap")
    }
}

impl<K, Q, V, S, A> IndexMut<&Q> for IndexMap<K, V, S, A>
where
    K: Hash + Eq + Borrow<Q>,
    Q: Hash + Eq + ?Sized,
    S: BuildHasher,
    A: Allocator,
{
    #[track_caller]
    fn index_mut(&mut self, key: &Q) -> &mut V {
        self.get_mut(key).expect("key not in IndexMap")
    }
}

impl<K: Hash + Eq, V, S: BuildHasher, A: Allocator> Extend<(K, V)> for IndexMap<K, V, S, A> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        let iter = iter.into_iter();
        self.reserve(iter.size_hint().0);
        for (k, v) in iter {
            self.insert(k, v);
        }
    }
}

impl<K: Hash + Eq, V, S: BuildHasher + Default> FromIterator<(K, V)> for IndexMap<K, V, S> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut m = Self::with_hasher(S::default());
        m.extend(iter);
        m
    }
}

impl<'a, K, V, S, A: Allocator> IntoIterator for &'a IndexMap<K, V, S, A> {
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V>;

    fn into_iter(self) -> Iter<'a, K, V> {
        self.iter()
    }
}

impl<'a, K, V, S, A: Allocator> IntoIterator for &'a mut IndexMap<K, V, S, A> {
    type Item = (&'a K, &'a mut V);
    type IntoIter = IterMut<'a, K, V>;

    fn into_iter(self) -> IterMut<'a, K, V> {
        self.iter_mut()
    }
}

impl<K, V, S, A: Allocator> IntoIterator for IndexMap<K, V, S, A> {
    type Item = (K, V);
    type IntoIter = IntoIter<K, V, A>;

    fn into_iter(self) -> IntoIter<K, V, A> {
        IntoIter {
            // Takes over the block: `vec::IntoIter` with an allocator is not stable yet.
            inner: SmallVec::from(self.entries).into_iter(),
        }
    }
}

/// Iterator over an [`IndexMap`]'s entries, in order.
pub struct Iter<'a, K, V> {
    inner: slice::Iter<'a, Bucket<K, V>>,
}

impl<K, V> Clone for Iter<'_, K, V> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|b| (&b.key, &b.value))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<K, V> DoubleEndedIterator for Iter<'_, K, V> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|b| (&b.key, &b.value))
    }
}

impl<K, V> ExactSizeIterator for Iter<'_, K, V> {}

impl<K, V> FusedIterator for Iter<'_, K, V> {}

/// Iterator over an [`IndexMap`]'s entries, in order, with the values mutable.
pub struct IterMut<'a, K, V> {
    inner: slice::IterMut<'a, Bucket<K, V>>,
}

impl<'a, K, V> Iterator for IterMut<'a, K, V> {
    type Item = (&'a K, &'a mut V);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|b| (&b.key, &mut b.value))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<K, V> DoubleEndedIterator for IterMut<'_, K, V> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|b| (&b.key, &mut b.value))
    }
}

impl<K, V> ExactSizeIterator for IterMut<'_, K, V> {}

impl<K, V> FusedIterator for IterMut<'_, K, V> {}

/// By-value iterator over an [`IndexMap`]'s entries, in order.
pub struct IntoIter<K, V, A: Allocator = Global> {
    inner: small_vec::IntoIter<Bucket<K, V>, 0, A>,
}

impl<K, V, A: Allocator> Iterator for IntoIter<K, V, A> {
    type Item = (K, V);

    #[inline]
    fn next(&mut self) -> Option<(K, V)> {
        self.inner.next().map(|b| (b.key, b.value))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<K, V, A: Allocator> DoubleEndedIterator for IntoIter<K, V, A> {
    #[inline]
    fn next_back(&mut self) -> Option<(K, V)> {
        self.inner.next_back().map(|b| (b.key, b.value))
    }
}

impl<K, V, A: Allocator> ExactSizeIterator for IntoIter<K, V, A> {}

impl<K, V, A: Allocator> FusedIterator for IntoIter<K, V, A> {}

// --- IndexSet ---

/// A hash set that remembers insertion order: an [`IndexMap`] with `()` values. The same two
/// removal flavors; equality ignores order.
pub struct IndexSet<T, S = FastState, A: Allocator = Global> {
    map: IndexMap<T, (), S, A>,
}

impl<T> IndexSet<T> {
    pub const fn new() -> Self {
        Self {
            map: IndexMap::new(),
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            map: IndexMap::with_capacity(capacity),
        }
    }
}

impl<T, S> IndexSet<T, S> {
    pub const fn with_hasher(hasher: S) -> Self {
        Self {
            map: IndexMap::with_hasher(hasher),
        }
    }
}

impl<T, A: Allocator + Clone> IndexSet<T, FastState, A> {
    pub fn new_in(alloc: A) -> Self {
        Self {
            map: IndexMap::new_in(alloc),
        }
    }
}

impl<T, S, A: Allocator + Clone> IndexSet<T, S, A> {
    pub fn with_capacity_and_hasher_in(capacity: usize, hasher: S, alloc: A) -> Self {
        Self {
            map: IndexMap::with_capacity_and_hasher_in(capacity, hasher, alloc),
        }
    }
}

impl<T, S, A: Allocator> IndexSet<T, S, A> {
    #[inline]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.map.capacity()
    }

    pub fn reserve(&mut self, additional: usize) {
        self.map.reserve(additional);
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// The element at `index`, in insertion order.
    #[inline]
    pub fn get_index(&self, index: usize) -> Option<&T> {
        self.map.get_index(index).map(|(k, _)| k)
    }

    #[inline]
    pub fn first(&self) -> Option<&T> {
        self.get_index(0)
    }

    #[inline]
    pub fn last(&self) -> Option<&T> {
        self.map.last().map(|(k, _)| k)
    }

    pub fn pop(&mut self) -> Option<T> {
        self.map.pop().map(|(k, _)| k)
    }

    pub fn swap_remove_index(&mut self, index: usize) -> Option<T> {
        self.map.swap_remove_index(index).map(|(k, _)| k)
    }

    pub fn shift_remove_index(&mut self, index: usize) -> Option<T> {
        self.map.shift_remove_index(index).map(|(k, _)| k)
    }

    pub fn retain(&mut self, mut f: impl FnMut(&T) -> bool) {
        self.map.retain(|k, _| f(k));
    }

    pub fn sort(&mut self)
    where
        T: Ord,
    {
        self.map.sort_keys();
    }

    pub fn sort_unstable_by(&mut self, mut cmp: impl FnMut(&T, &T) -> Ordering) {
        self.map.sort_unstable_by(|a, _, b, _| cmp(a, b));
    }

    pub fn reverse(&mut self) {
        self.map.reverse();
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &T> + DoubleEndedIterator + Clone + '_ {
        self.map.iter().map(|(k, _)| k)
    }
}

impl<T: Hash + Eq, S: BuildHasher, A: Allocator> IndexSet<T, S, A> {
    /// Adds `value`; returns whether it was new. An existing element keeps its position (and the
    /// new value is dropped).
    #[inline]
    pub fn insert(&mut self, value: T) -> bool {
        self.insert_full(value).1
    }

    /// [`insert`](Self::insert), also returning the element's position.
    pub fn insert_full(&mut self, value: T) -> (usize, bool) {
        let hash = self.map.hasher.hash_one(&value);
        let entries = &self.map.entries;
        match self.map.indices.find(hash, |&i| entries[i].key == value) {
            Some(&i) => (i, false),
            None => (self.map.push(hash, value, ()), true),
        }
    }

    #[inline]
    pub fn contains<Q: Hash + Eq + ?Sized>(&self, value: &Q) -> bool
    where
        T: Borrow<Q>,
    {
        self.map.contains_key(value)
    }

    #[inline]
    pub fn get_index_of<Q: Hash + Eq + ?Sized>(&self, value: &Q) -> Option<usize>
    where
        T: Borrow<Q>,
    {
        self.map.get_index_of(value)
    }

    /// The stored element equal to `value`.
    pub fn get<Q: Hash + Eq + ?Sized>(&self, value: &Q) -> Option<&T>
    where
        T: Borrow<Q>,
    {
        self.map.get_full(value).map(|(_, k, _)| k)
    }

    /// Removes `value`, moving the last element into its place. O(1).
    pub fn swap_remove<Q: Hash + Eq + ?Sized>(&mut self, value: &Q) -> bool
    where
        T: Borrow<Q>,
    {
        self.map.swap_remove(value).is_some()
    }

    /// Removes `value`, shifting the later elements down. O(n); keeps the order.
    pub fn shift_remove<Q: Hash + Eq + ?Sized>(&mut self, value: &Q) -> bool
    where
        T: Borrow<Q>,
    {
        self.map.shift_remove(value).is_some()
    }
}

impl<T, S: Default> Default for IndexSet<T, S> {
    fn default() -> Self {
        Self::with_hasher(S::default())
    }
}

impl<T: Clone, S: Clone, A: Allocator + Clone> Clone for IndexSet<T, S, A> {
    fn clone(&self) -> Self {
        Self {
            map: self.map.clone(),
        }
    }
}

impl<T: fmt::Debug, S, A: Allocator> fmt::Debug for IndexSet<T, S, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl<T, S1, S2, A1, A2> PartialEq<IndexSet<T, S2, A2>> for IndexSet<T, S1, A1>
where
    T: Hash + Eq,
    S1: BuildHasher,
    S2: BuildHasher,
    A1: Allocator,
    A2: Allocator,
{
    /// The same elements, in any order.
    fn eq(&self, other: &IndexSet<T, S2, A2>) -> bool {
        self.map == other.map
    }
}

impl<T: Hash + Eq, S: BuildHasher, A: Allocator> Eq for IndexSet<T, S, A> {}

impl<T: Hash + Eq, S: BuildHasher, A: Allocator> Extend<T> for IndexSet<T, S, A> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        self.map.extend(iter.into_iter().map(|t| (t, ())));
    }
}

impl<T: Hash + Eq, S: BuildHasher + Default> FromIterator<T> for IndexSet<T, S> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut s = Self::with_hasher(S::default());
        s.extend(iter);
        s
    }
}

impl<T, S, A: Allocator> IntoIterator for IndexSet<T, S, A> {
    type Item = T;
    type IntoIter = core::iter::Map<IntoIter<T, (), A>, fn((T, ())) -> T>;

    fn into_iter(self) -> Self::IntoIter {
        let key: fn((T, ())) -> T = |(k, ())| k;
        self.map.into_iter().map(key)
    }
}
