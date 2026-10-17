//! `SecondaryMap<K, V, A>`: extra values for the handles of a `SlotMap<K>`.

use alloc::alloc::{Allocator, Global};
use alloc::vec::Vec;
use core::fmt;
use core::marker::PhantomData;
use core::ops::{Index, IndexMut};

use crate::Handle;

struct Slot<V> {
    /// Generation of the handle the value belongs to; meaningless while `value` is `None`.
    generation: u32,
    value: Option<V>,
}

/// Values attached to the handles of a [`SlotMap<K>`](crate::SlotMap), stored densely by slot
/// index: lookup is one array read plus a generation check, as in the slot map itself. Use it to
/// give some of a slot map's elements extra data (a component, a cache) without touching `K`.
///
/// Stale handles behave as in the slot map: a value inserted for an old occupant of a slot is
/// invisible to (and replaced by) the newer occupant's handle, and a handle older than the
/// stored value inserts nothing. Memory grows with the highest slot index used.
pub struct SecondaryMap<K, V, A: Allocator = Global> {
    slots: Vec<Slot<V>, A>,
    len: usize,
    _key: PhantomData<fn() -> K>,
}

impl<K, V> SecondaryMap<K, V> {
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            len: 0,
            _key: PhantomData,
        }
    }

    /// Room for slot indices below `capacity` without reallocating.
    pub fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity_in(capacity, Global)
    }
}

impl<K, V, A: Allocator> SecondaryMap<K, V, A> {
    pub fn new_in(alloc: A) -> Self {
        Self::with_capacity_in(0, alloc)
    }

    pub fn with_capacity_in(capacity: usize, alloc: A) -> Self {
        Self {
            slots: Vec::with_capacity_in(capacity, alloc),
            len: 0,
            _key: PhantomData,
        }
    }

    #[inline]
    pub fn allocator(&self) -> &A {
        self.slots.allocator()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Sets `handle`'s value; returns the previous value for this same handle. A null handle, or
    /// one older than the value already stored in its slot, drops `value` and returns `None`.
    pub fn insert(&mut self, handle: Handle<K>, value: V) -> Option<V> {
        let old = self.slot_for(handle)?.value.replace(value);
        if old.is_none() {
            self.len += 1;
        }
        old
    }

    /// The value for `handle`, inserting `f()` if there is none. `None` for a null or stale
    /// handle (see [`insert`](Self::insert)).
    pub fn get_or_insert_with(
        &mut self,
        handle: Handle<K>,
        f: impl FnOnce() -> V,
    ) -> Option<&mut V> {
        let index = handle.index() as usize;
        if self.slot_for(handle)?.value.is_none() {
            let value = f();
            self.slots[index].value = Some(value);
            self.len += 1;
        }
        self.slots[index].value.as_mut()
    }

    /// The slot for `handle`, with its generation, cleared of any older occupant's value; `None`
    /// if `handle` is null or older than the slot's value.
    fn slot_for(&mut self, handle: Handle<K>) -> Option<&mut Slot<V>> {
        if handle.is_null() {
            return None;
        }
        let index = handle.index() as usize;
        if index >= self.slots.len() {
            self.slots.resize_with(index + 1, || Slot {
                generation: 0,
                value: None,
            });
        }
        let slot = &mut self.slots[index];
        if slot.value.is_some() {
            if slot.generation > handle.generation() {
                return None;
            }
            if slot.generation < handle.generation() {
                // A value for an earlier occupant of the slot: gone with it.
                slot.value = None;
                self.len -= 1;
            }
        }
        slot.generation = handle.generation();
        Some(slot)
    }

    /// Removes and returns `handle`'s value.
    pub fn remove(&mut self, handle: Handle<K>) -> Option<V> {
        let slot = self.slots.get_mut(handle.index() as usize)?;
        if slot.generation != handle.generation() {
            return None;
        }
        let value = slot.value.take()?;
        self.len -= 1;
        Some(value)
    }

    #[inline]
    pub fn contains(&self, handle: Handle<K>) -> bool {
        self.get(handle).is_some()
    }

    #[inline]
    pub fn get(&self, handle: Handle<K>) -> Option<&V> {
        match self.slots.get(handle.index() as usize) {
            Some(Slot {
                generation,
                value: Some(v),
            }) if *generation == handle.generation() => Some(v),
            _ => None,
        }
    }

    #[inline]
    pub fn get_mut(&mut self, handle: Handle<K>) -> Option<&mut V> {
        match self.slots.get_mut(handle.index() as usize) {
            Some(Slot {
                generation,
                value: Some(v),
            }) if *generation == handle.generation() => Some(v),
            _ => None,
        }
    }

    /// Removes every value; keeps the memory.
    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            slot.value = None;
        }
        self.len = 0;
    }

    /// Keeps the values for which `f` returns true.
    pub fn retain(&mut self, mut f: impl FnMut(Handle<K>, &mut V) -> bool) {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if let Some(v) = &mut slot.value
                && !f(Handle::new(i as u32, slot.generation), v)
            {
                slot.value = None;
                self.len -= 1;
            }
        }
    }

    /// `(handle, &value)` pairs in slot order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (Handle<K>, &V)> + '_ {
        self.slots.iter().enumerate().filter_map(|(i, slot)| {
            let v = slot.value.as_ref()?;
            Some((Handle::new(i as u32, slot.generation), v))
        })
    }

    /// `(handle, &mut value)` pairs in slot order.
    pub fn iter_mut(&mut self) -> impl DoubleEndedIterator<Item = (Handle<K>, &mut V)> + '_ {
        self.slots.iter_mut().enumerate().filter_map(|(i, slot)| {
            let generation = slot.generation;
            let v = slot.value.as_mut()?;
            Some((Handle::new(i as u32, generation), v))
        })
    }

    pub fn handles(&self) -> impl DoubleEndedIterator<Item = Handle<K>> + '_ {
        self.iter().map(|(h, _)| h)
    }

    pub fn values(&self) -> impl DoubleEndedIterator<Item = &V> + '_ {
        self.slots.iter().filter_map(|s| s.value.as_ref())
    }

    pub fn values_mut(&mut self) -> impl DoubleEndedIterator<Item = &mut V> + '_ {
        self.slots.iter_mut().filter_map(|s| s.value.as_mut())
    }
}

impl<K, V> Default for SecondaryMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V: Clone, A: Allocator + Clone> Clone for SecondaryMap<K, V, A> {
    fn clone(&self) -> Self {
        let mut slots = Vec::with_capacity_in(self.slots.len(), self.allocator().clone());
        slots.extend(self.slots.iter().map(|s| Slot {
            generation: s.generation,
            value: s.value.clone(),
        }));
        Self {
            slots,
            len: self.len,
            _key: PhantomData,
        }
    }
}

impl<K, V: fmt::Debug, A: Allocator> fmt::Debug for SecondaryMap<K, V, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K, V, A: Allocator> Index<Handle<K>> for SecondaryMap<K, V, A> {
    type Output = V;

    /// Panics if `handle` has no value.
    #[track_caller]
    fn index(&self, handle: Handle<K>) -> &V {
        match self.get(handle) {
            Some(v) => v,
            None => panic!("no value for {handle:?}"),
        }
    }
}

impl<K, V, A: Allocator> IndexMut<Handle<K>> for SecondaryMap<K, V, A> {
    #[track_caller]
    fn index_mut(&mut self, handle: Handle<K>) -> &mut V {
        match self.get_mut(handle) {
            Some(v) => v,
            None => panic!("no value for {handle:?}"),
        }
    }
}

impl<K, V, A: Allocator> Extend<(Handle<K>, V)> for SecondaryMap<K, V, A> {
    fn extend<I: IntoIterator<Item = (Handle<K>, V)>>(&mut self, iter: I) {
        for (h, v) in iter {
            self.insert(h, v);
        }
    }
}

impl<K, V> FromIterator<(Handle<K>, V)> for SecondaryMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (Handle<K>, V)>>(iter: I) -> Self {
        let mut m = Self::new();
        m.extend(iter);
        m
    }
}
