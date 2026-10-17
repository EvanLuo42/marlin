//! `SlotMap<T>`: storage addressed by generational handles.

use alloc::alloc::{Allocator, Global};
use alloc::vec::Vec;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;
use core::ops::{Index, IndexMut};
use core::{fmt, mem};

/// A key into a [`SlotMap<T>`]: a slot index plus the generation the slot had when the value was
/// inserted. Removing a value bumps the slot's generation, so old handles stop matching and
/// return `None` instead of aliasing whatever reuses the slot.
///
/// 8 bytes, `Copy`, and typed: a `Handle<Mesh>` cannot index a `SlotMap<Texture>`.
pub struct Handle<T> {
    index: u32,
    generation: u32,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Handle<T> {
    /// A handle that never refers to anything (generation 0 is never issued).
    pub const NULL: Self = Self::new(u32::MAX, 0);

    #[inline]
    pub(crate) const fn new(index: u32, generation: u32) -> Self {
        Self {
            index,
            generation,
            _marker: PhantomData,
        }
    }

    #[inline]
    pub const fn index(self) -> u32 {
        self.index
    }

    #[inline]
    pub const fn generation(self) -> u32 {
        self.generation
    }

    #[inline]
    pub const fn is_null(self) -> bool {
        self.generation == 0
    }

    /// Packs into a `u64` (generation in the high half), e.g. to cross an FFI or VM boundary.
    #[inline]
    pub const fn to_bits(self) -> u64 {
        ((self.generation as u64) << 32) | self.index as u64
    }

    /// Inverse of [`to_bits`](Self::to_bits). Any value is safe: a bogus handle just misses.
    #[inline]
    pub const fn from_bits(bits: u64) -> Self {
        Self::new(bits as u32, (bits >> 32) as u32)
    }
}

// Manual impls: derives would wrongly require `T: Clone` etc.
impl<T> Clone for Handle<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Handle<T> {}

impl<T> PartialEq for Handle<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.to_bits() == other.to_bits()
    }
}

impl<T> Eq for Handle<T> {}

impl<T> PartialOrd for Handle<T> {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Handle<T> {
    #[inline]
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.to_bits().cmp(&other.to_bits())
    }
}

impl<T> Hash for Handle<T> {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.to_bits());
    }
}

impl<T> Default for Handle<T> {
    #[inline]
    fn default() -> Self {
        Self::NULL
    }
}

impl<T> fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_null() {
            f.write_str("Handle(null)")
        } else {
            write!(f, "Handle({}v{})", self.index, self.generation)
        }
    }
}

/// End of the free list.
const NONE: u32 = u32::MAX;

struct Slot<T> {
    /// Differs from every handle issued for an earlier occupant. Starts at 1, never 0.
    generation: u32,
    value: Entry<T>,
}

enum Entry<T> {
    Occupied(T),
    Vacant { next_free: u32 },
}

/// Values addressed by [`Handle<T>`]. Insert, remove and lookup are O(1); freed slots are reused
/// LIFO. Values are stored contiguously, so iteration is a linear scan (skipping vacant slots).
///
/// A slot whose generation would wrap is retired instead of reused, so a handle can never be
/// confused with a later occupant even after 2^32 reuses.
///
/// The slots live in one block from `A`.
pub struct SlotMap<T, A: Allocator = Global> {
    slots: Vec<Slot<T>, A>,
    free_head: u32,
    len: u32,
}

impl<T> SlotMap<T> {
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free_head: NONE,
            len: 0,
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity_in(capacity, Global)
    }
}

impl<T, A: Allocator> SlotMap<T, A> {
    pub fn new_in(alloc: A) -> Self {
        Self {
            slots: Vec::new_in(alloc),
            free_head: NONE,
            len: 0,
        }
    }

    pub fn with_capacity_in(capacity: usize, alloc: A) -> Self {
        Self {
            slots: Vec::with_capacity_in(capacity, alloc),
            free_head: NONE,
            len: 0,
        }
    }

    #[inline]
    pub fn allocator(&self) -> &A {
        self.slots.allocator()
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Slots allocated (occupied, free or retired).
    #[inline]
    pub fn capacity(&self) -> usize {
        self.slots.capacity()
    }

    /// Makes room for `additional` more values without reallocating.
    pub fn reserve(&mut self, additional: usize) {
        let free = self.slots.len() - self.len as usize;
        self.slots.reserve(additional.saturating_sub(free));
    }

    /// Inserts `value` and returns its handle.
    pub fn insert(&mut self, value: T) -> Handle<T> {
        self.insert_with_handle(|_| value)
    }

    /// Inserts the value `f` builds from its own handle (for values that store their handle).
    pub fn insert_with_handle(&mut self, f: impl FnOnce(Handle<T>) -> T) -> Handle<T> {
        if self.free_head != NONE {
            let index = self.free_head;
            let slot = &mut self.slots[index as usize];
            let Entry::Vacant { next_free } = slot.value else {
                unreachable!("free list points at an occupied slot");
            };
            let handle = Handle::new(index, slot.generation);
            slot.value = Entry::Occupied(f(handle));
            self.free_head = next_free;
            self.len += 1;
            handle
        } else {
            // u32::MAX is reserved for `Handle::NULL` and the free-list terminator.
            let index = u32::try_from(self.slots.len())
                .ok()
                .filter(|&i| i != u32::MAX)
                .expect("SlotMap is full");
            let handle = Handle::new(index, 1);
            self.slots.push(Slot {
                generation: 1,
                value: Entry::Occupied(f(handle)),
            });
            self.len += 1;
            handle
        }
    }

    /// Removes and returns the value, or `None` if the handle is stale.
    pub fn remove(&mut self, handle: Handle<T>) -> Option<T> {
        let slot = self.slots.get_mut(handle.index as usize)?;
        if slot.generation != handle.generation || matches!(slot.value, Entry::Vacant { .. }) {
            return None;
        }
        let value = self.vacate(handle.index);
        self.len -= 1;
        Some(value)
    }

    /// Frees an occupied slot: bumps its generation and pushes it on the free list (or retires it
    /// if the generation is exhausted).
    fn vacate(&mut self, index: u32) -> T {
        let slot = &mut self.slots[index as usize];
        let (generation, next_free, free_head) = match slot.generation.checked_add(1) {
            Some(g) => (g, self.free_head, index),
            // Retired: never handed out again.
            None => (slot.generation, NONE, self.free_head),
        };
        slot.generation = generation;
        self.free_head = free_head;
        match mem::replace(&mut slot.value, Entry::Vacant { next_free }) {
            Entry::Occupied(v) => v,
            Entry::Vacant { .. } => unreachable!("vacated a free slot"),
        }
    }

    #[inline]
    pub fn contains(&self, handle: Handle<T>) -> bool {
        self.get(handle).is_some()
    }

    #[inline]
    pub fn get(&self, handle: Handle<T>) -> Option<&T> {
        match self.slots.get(handle.index as usize) {
            Some(Slot {
                generation,
                value: Entry::Occupied(v),
            }) if *generation == handle.generation => Some(v),
            _ => None,
        }
    }

    #[inline]
    pub fn get_mut(&mut self, handle: Handle<T>) -> Option<&mut T> {
        match self.slots.get_mut(handle.index as usize) {
            Some(Slot {
                generation,
                value: Entry::Occupied(v),
            }) if *generation == handle.generation => Some(v),
            _ => None,
        }
    }

    /// Removes every value. All outstanding handles become stale.
    pub fn clear(&mut self) {
        self.retain(|_, _| false);
    }

    /// Keeps the values for which `f` returns true.
    pub fn retain(&mut self, mut f: impl FnMut(Handle<T>, &mut T) -> bool) {
        for index in 0..self.slots.len() as u32 {
            let slot = &mut self.slots[index as usize];
            let Entry::Occupied(v) = &mut slot.value else {
                continue;
            };
            if !f(Handle::new(index, slot.generation), v) {
                drop(self.vacate(index));
                self.len -= 1;
            }
        }
    }

    /// `(handle, &value)` pairs in slot order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (Handle<T>, &T)> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| match &slot.value {
                Entry::Occupied(v) => Some((Handle::new(i as u32, slot.generation), v)),
                Entry::Vacant { .. } => None,
            })
    }

    /// `(handle, &mut value)` pairs in slot order.
    pub fn iter_mut(&mut self) -> impl DoubleEndedIterator<Item = (Handle<T>, &mut T)> + '_ {
        self.slots
            .iter_mut()
            .enumerate()
            .filter_map(|(i, slot)| match &mut slot.value {
                Entry::Occupied(v) => Some((Handle::new(i as u32, slot.generation), v)),
                Entry::Vacant { .. } => None,
            })
    }

    pub fn handles(&self) -> impl DoubleEndedIterator<Item = Handle<T>> + '_ {
        self.iter().map(|(h, _)| h)
    }

    pub fn values(&self) -> impl DoubleEndedIterator<Item = &T> + '_ {
        self.iter().map(|(_, v)| v)
    }

    pub fn values_mut(&mut self) -> impl DoubleEndedIterator<Item = &mut T> + '_ {
        self.iter_mut().map(|(_, v)| v)
    }
}

impl<T> Default for SlotMap<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone, A: Allocator + Clone> Clone for SlotMap<T, A> {
    /// Handles into the original are valid in the clone too.
    fn clone(&self) -> Self {
        let mut slots = Vec::with_capacity_in(self.slots.len(), self.allocator().clone());
        slots.extend(self.slots.iter().map(|s| Slot {
            generation: s.generation,
            value: match &s.value {
                Entry::Occupied(v) => Entry::Occupied(v.clone()),
                Entry::Vacant { next_free } => Entry::Vacant {
                    next_free: *next_free,
                },
            },
        }));
        Self {
            slots,
            free_head: self.free_head,
            len: self.len,
        }
    }
}

impl<T: fmt::Debug, A: Allocator> fmt::Debug for SlotMap<T, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<T, A: Allocator> Index<Handle<T>> for SlotMap<T, A> {
    type Output = T;

    /// Panics on a stale handle.
    #[track_caller]
    fn index(&self, handle: Handle<T>) -> &T {
        match self.get(handle) {
            Some(v) => v,
            None => panic!("stale or invalid {handle:?}"),
        }
    }
}

impl<T, A: Allocator> IndexMut<Handle<T>> for SlotMap<T, A> {
    #[track_caller]
    fn index_mut(&mut self, handle: Handle<T>) -> &mut T {
        match self.get_mut(handle) {
            Some(v) => v,
            None => panic!("stale or invalid {handle:?}"),
        }
    }
}
