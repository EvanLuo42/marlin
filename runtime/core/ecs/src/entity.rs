//! Entity ids and their locations in archetype storage.

use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicIsize, Ordering};

use crate::archetype::ArchetypeId;
use crate::storage::TableId;

/// A generational id: an index into the world's entity table plus the generation it was
/// allocated in, so ids of despawned entities never alias new ones.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Entity {
    index: u32,
    generation: u32,
}

impl Entity {
    #[inline]
    pub const fn index(self) -> u32 {
        self.index
    }

    #[inline]
    pub const fn generation(self) -> u32 {
        self.generation
    }

    /// Generation in the high half, index in the low half: stable for serialization within a run.
    #[inline]
    pub const fn to_bits(self) -> u64 {
        (self.generation as u64) << 32 | self.index as u64
    }

    #[inline]
    pub const fn from_bits(bits: u64) -> Self {
        Self {
            index: bits as u32,
            generation: (bits >> 32) as u32,
        }
    }
}

impl fmt::Debug for Entity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Entity({}v{})", self.index, self.generation)
    }
}

/// Where an entity's components live: its archetype and row there, and its table and row there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct EntityLocation {
    pub(crate) archetype: ArchetypeId,
    pub(crate) archetype_row: u32,
    pub(crate) table: TableId,
    pub(crate) table_row: u32,
}

impl EntityLocation {
    pub(crate) const FREE: Self = Self {
        archetype: ArchetypeId::INVALID,
        archetype_row: u32::MAX,
        table: TableId::INVALID,
        table_row: u32::MAX,
    };
}

struct Meta {
    generation: u32,
    location: EntityLocation,
}

/// The entity table. Ids can be reserved through a shared borrow ([`reserve`](Self::reserve)), so
/// [`Commands`](crate::Commands) can hand out ids while systems run; reserved ids become live,
/// component-less entities at the next [`flush`](Self::flush).
///
/// Reservation follows `free_cursor`: while it is positive, it counts down through `pending` (the
/// free list); below zero, each step names a fresh index past the end of `meta`. A flush settles
/// both and resets it to `pending.len()`.
pub(crate) struct Entities {
    meta: Vec<Meta>,
    pending: Vec<u32>,
    free_cursor: AtomicIsize,
    alive: u32,
}

impl Default for Entities {
    fn default() -> Self {
        Self {
            meta: Vec::new(),
            pending: Vec::new(),
            free_cursor: AtomicIsize::new(0),
            alive: 0,
        }
    }
}

impl Entities {
    /// An id no one else holds, usable before the next flush makes it live.
    pub(crate) fn reserve(&self) -> Entity {
        let n = self.free_cursor.fetch_sub(1, Ordering::Relaxed);
        if n > 0 {
            let index = self.pending[n as usize - 1];
            Entity {
                index,
                generation: self.meta[index as usize].generation,
            }
        } else {
            let index = self.meta.len() as isize - n;
            Entity {
                index: u32::try_from(index).expect("entity index overflow"),
                generation: 0,
            }
        }
    }

    #[inline]
    pub(crate) fn needs_flush(&mut self) -> bool {
        *self.free_cursor.get_mut() != self.pending.len() as isize
    }

    /// Makes every reserved id live, calling `init` to place each one.
    pub(crate) fn flush(&mut self, mut init: impl FnMut(Entity) -> EntityLocation) {
        let cursor = *self.free_cursor.get_mut();
        if cursor == self.pending.len() as isize {
            return;
        }
        let first_reused = cursor.max(0) as usize;
        let fresh = if cursor < 0 { cursor.unsigned_abs() } else { 0 };
        for index in self.pending.drain(first_reused..) {
            let meta = &mut self.meta[index as usize];
            let entity = Entity {
                index,
                generation: meta.generation,
            };
            meta.location = init(entity);
            self.alive += 1;
        }
        for _ in 0..fresh {
            let index = u32::try_from(self.meta.len()).expect("entity index overflow");
            let entity = Entity {
                index,
                generation: 0,
            };
            self.meta.push(Meta {
                generation: 0,
                location: EntityLocation::FREE,
            });
            self.meta[index as usize].location = init(entity);
            self.alive += 1;
        }
        *self.free_cursor.get_mut() = self.pending.len() as isize;
    }

    /// A fresh id; its location must be set before anything looks it up. Requires a flushed table.
    pub(crate) fn alloc(&mut self) -> Entity {
        debug_assert!(!self.needs_flush());
        self.alive += 1;
        let entity = if let Some(index) = self.pending.pop() {
            Entity {
                index,
                generation: self.meta[index as usize].generation,
            }
        } else {
            let index = u32::try_from(self.meta.len()).expect("entity index overflow");
            self.meta.push(Meta {
                generation: 0,
                location: EntityLocation::FREE,
            });
            Entity {
                index,
                generation: 0,
            }
        };
        *self.free_cursor.get_mut() = self.pending.len() as isize;
        entity
    }

    /// Room for `additional` more entities without reallocating.
    pub(crate) fn reserve_capacity(&mut self, additional: usize) {
        self.meta
            .reserve(additional.saturating_sub(self.pending.len()));
    }

    /// Frees a live entity's id; its index is reused with the next generation. Requires a flushed
    /// table.
    pub(crate) fn free(&mut self, entity: Entity) {
        debug_assert!(!self.needs_flush());
        let meta = &mut self.meta[entity.index as usize];
        debug_assert_eq!(meta.generation, entity.generation);
        meta.generation = meta.generation.wrapping_add(1);
        meta.location = EntityLocation::FREE;
        self.pending.push(entity.index);
        *self.free_cursor.get_mut() = self.pending.len() as isize;
        self.alive -= 1;
    }

    #[inline]
    pub(crate) fn location(&self, entity: Entity) -> Option<EntityLocation> {
        self.meta
            .get(entity.index as usize)
            .filter(|m| m.generation == entity.generation && m.location != EntityLocation::FREE)
            .map(|m| m.location)
    }

    #[inline]
    pub(crate) fn set_location(&mut self, entity: Entity, location: EntityLocation) {
        self.meta[entity.index as usize].location = location;
    }

    /// The location of a live entity, for fixing up rows.
    #[inline]
    pub(crate) fn location_mut(&mut self, entity: Entity) -> &mut EntityLocation {
        &mut self.meta[entity.index as usize].location
    }

    pub(crate) fn alive(&self) -> u32 {
        self.alive
    }
}
