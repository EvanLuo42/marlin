//! Arenas, typed `u32` indices and interning for the Marlin language toolchain
//! (`docs/design/0015-language-frontend.md` §1, `0006-shader-ir.md` §12).
//!
//! Compiler data structures refer to each other by [`Idx`] instead of references or `Rc`, so
//! they are plain data: cheap to clone, compare and hash, with no lifetimes or unsafe pointer
//! graphs.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::iter::FusedIterator;
use std::marker::PhantomData;
use std::ops::{Index, IndexMut, Range};

/// An index of a `T` in some [`Arena<T>`].
pub struct Idx<T> {
    raw: u32,
    _ty: PhantomData<fn() -> T>,
}

impl<T> Idx<T> {
    pub const fn from_raw(raw: u32) -> Self {
        Self {
            raw,
            _ty: PhantomData,
        }
    }

    pub const fn into_raw(self) -> u32 {
        self.raw
    }

    pub const fn index(self) -> usize {
        self.raw as usize
    }
}

impl<T> Clone for Idx<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Idx<T> {}

impl<T> PartialEq for Idx<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<T> Eq for Idx<T> {}

impl<T> PartialOrd for Idx<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Idx<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.raw.cmp(&other.raw)
    }
}

impl<T> Hash for Idx<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl<T> fmt::Debug for Idx<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = std::any::type_name::<T>();
        let name = name.rsplit("::").next().unwrap_or(name);
        write!(f, "Idx::<{name}>({})", self.raw)
    }
}

/// A contiguous run of indices `start..end` allocated together.
pub struct IdxRange<T> {
    range: Range<u32>,
    _ty: PhantomData<fn() -> T>,
}

impl<T> IdxRange<T> {
    pub fn new(range: Range<Idx<T>>) -> Self {
        Self {
            range: range.start.raw..range.end.raw,
            _ty: PhantomData,
        }
    }

    pub fn len(&self) -> usize {
        self.range.len()
    }

    pub fn is_empty(&self) -> bool {
        self.range.is_empty()
    }
}

impl<T> Clone for IdxRange<T> {
    fn clone(&self) -> Self {
        Self {
            range: self.range.clone(),
            _ty: PhantomData,
        }
    }
}

impl<T> PartialEq for IdxRange<T> {
    fn eq(&self, other: &Self) -> bool {
        self.range == other.range
    }
}

impl<T> Eq for IdxRange<T> {}

impl<T> fmt::Debug for IdxRange<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IdxRange({:?})", self.range)
    }
}

impl<T> Iterator for IdxRange<T> {
    type Item = Idx<T>;
    fn next(&mut self) -> Option<Idx<T>> {
        self.range.next().map(Idx::from_raw)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.range.size_hint()
    }
}

impl<T> DoubleEndedIterator for IdxRange<T> {
    fn next_back(&mut self) -> Option<Idx<T>> {
        self.range.next_back().map(Idx::from_raw)
    }
}

impl<T> ExactSizeIterator for IdxRange<T> {}
impl<T> FusedIterator for IdxRange<T> {}

/// An append-only vector addressed by [`Idx`].
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Arena<T> {
    data: Vec<T>,
}

impl<T> Arena<T> {
    pub const fn new() -> Self {
        Self { data: Vec::new() }
    }

    /// # Panics
    /// If the arena already holds `u32::MAX` values.
    pub fn alloc(&mut self, value: T) -> Idx<T> {
        let raw = u32::try_from(self.data.len()).expect("arena index overflow");
        self.data.push(value);
        Idx::from_raw(raw)
    }

    /// Allocates every value of `values` and returns their contiguous range.
    pub fn alloc_many(&mut self, values: impl IntoIterator<Item = T>) -> IdxRange<T> {
        let start = self.next_idx();
        self.data.extend(values);
        IdxRange::new(start..self.next_idx())
    }

    /// The index the next `alloc` will return.
    pub fn next_idx(&self) -> Idx<T> {
        Idx::from_raw(self.data.len() as u32)
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn get(&self, idx: Idx<T>) -> Option<&T> {
        self.data.get(idx.index())
    }

    pub fn get_mut(&mut self, idx: Idx<T>) -> Option<&mut T> {
        self.data.get_mut(idx.index())
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (Idx<T>, &T)> + DoubleEndedIterator {
        self.data
            .iter()
            .enumerate()
            .map(|(i, v)| (Idx::from_raw(i as u32), v))
    }

    pub fn iter_mut(
        &mut self,
    ) -> impl ExactSizeIterator<Item = (Idx<T>, &mut T)> + DoubleEndedIterator {
        self.data
            .iter_mut()
            .enumerate()
            .map(|(i, v)| (Idx::from_raw(i as u32), v))
    }

    pub fn values(&self) -> std::slice::Iter<'_, T> {
        self.data.iter()
    }

    pub fn shrink_to_fit(&mut self) {
        self.data.shrink_to_fit();
    }
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: fmt::Debug> fmt::Debug for Arena<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.data.iter().enumerate()).finish()
    }
}

impl<T> Index<Idx<T>> for Arena<T> {
    type Output = T;
    fn index(&self, idx: Idx<T>) -> &T {
        &self.data[idx.index()]
    }
}

impl<T> IndexMut<Idx<T>> for Arena<T> {
    fn index_mut(&mut self, idx: Idx<T>) -> &mut T {
        &mut self.data[idx.index()]
    }
}

/// Side data for some of an arena's values, addressed by the same [`Idx`].
#[derive(Clone, PartialEq, Eq)]
pub struct ArenaMap<T, V> {
    values: Vec<Option<V>>,
    _ty: PhantomData<fn() -> T>,
}

impl<T, V> ArenaMap<T, V> {
    pub const fn new() -> Self {
        Self {
            values: Vec::new(),
            _ty: PhantomData,
        }
    }

    /// Sets the value for `idx` and returns the previous one.
    pub fn insert(&mut self, idx: Idx<T>, value: V) -> Option<V> {
        if self.values.len() <= idx.index() {
            self.values.resize_with(idx.index() + 1, || None);
        }
        self.values[idx.index()].replace(value)
    }

    pub fn remove(&mut self, idx: Idx<T>) -> Option<V> {
        self.values.get_mut(idx.index()).and_then(Option::take)
    }

    pub fn get(&self, idx: Idx<T>) -> Option<&V> {
        self.values.get(idx.index()).and_then(Option::as_ref)
    }

    pub fn get_mut(&mut self, idx: Idx<T>) -> Option<&mut V> {
        self.values.get_mut(idx.index()).and_then(Option::as_mut)
    }

    pub fn contains(&self, idx: Idx<T>) -> bool {
        self.get(idx).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = (Idx<T>, &V)> {
        self.values
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.as_ref().map(|v| (Idx::from_raw(i as u32), v)))
    }
}

impl<T, V> Default for ArenaMap<T, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, V: fmt::Debug> fmt::Debug for ArenaMap<T, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.iter().map(|(i, v)| (i.raw, v)))
            .finish()
    }
}

impl<T, V> Index<Idx<T>> for ArenaMap<T, V> {
    type Output = V;
    fn index(&self, idx: Idx<T>) -> &V {
        self.get(idx).expect("no value for this index")
    }
}

/// Deduplicates values: equal values get the same [`Idx`].
#[derive(Clone)]
pub struct Interner<T: Eq + Hash> {
    arena: Arena<T>,
    lookup: HashMap<T, Idx<T>>,
}

impl<T: Eq + Hash + Clone> Interner<T> {
    pub fn new() -> Self {
        Self {
            arena: Arena::new(),
            lookup: HashMap::new(),
        }
    }

    /// The index of `value`, adding it if it is new.
    pub fn intern<Q>(&mut self, value: &Q) -> Idx<T>
    where
        T: Borrow<Q>,
        Q: Eq + Hash + ToOwned<Owned = T> + ?Sized,
    {
        if let Some(&idx) = self.lookup.get(value) {
            return idx;
        }
        let owned = value.to_owned();
        let idx = self.arena.alloc(owned.clone());
        self.lookup.insert(owned, idx);
        idx
    }

    /// The index of `value` if it has been interned.
    pub fn lookup<Q>(&self, value: &Q) -> Option<Idx<T>>
    where
        T: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.lookup.get(value).copied()
    }

    pub fn len(&self) -> usize {
        self.arena.len()
    }

    pub fn is_empty(&self) -> bool {
        self.arena.is_empty()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (Idx<T>, &T)> {
        self.arena.iter()
    }
}

impl<T: Eq + Hash + Clone> Default for Interner<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Eq + Hash> Index<Idx<T>> for Interner<T> {
    type Output = T;
    fn index(&self, idx: Idx<T>) -> &T {
        &self.arena[idx]
    }
}

impl<T: Eq + Hash + fmt::Debug> fmt::Debug for Interner<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.arena.fmt(f)
    }
}
