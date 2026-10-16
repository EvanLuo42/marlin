//! Entity ids and their locations in archetype storage.

use alloc::vec::Vec;
use core::fmt;

use crate::archetype::ArchetypeId;

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

/// Where an entity's components live: its archetype and its row in every column.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct EntityLocation {
    pub(crate) archetype: ArchetypeId,
    pub(crate) row: u32,
}

impl EntityLocation {
    const FREE: Self = Self {
        archetype: ArchetypeId::INVALID,
        row: u32::MAX,
    };
}

struct Meta {
    generation: u32,
    location: EntityLocation,
}

#[derive(Default)]
pub(crate) struct Entities {
    meta: Vec<Meta>,
    free: Vec<u32>,
    alive: u32,
}

impl Entities {
    /// A fresh id; its location must be set before anything looks it up.
    pub(crate) fn alloc(&mut self) -> Entity {
        self.alive += 1;
        if let Some(index) = self.free.pop() {
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
        }
    }

    /// Frees a live entity's id; its index is reused with the next generation.
    pub(crate) fn free(&mut self, entity: Entity) {
        let meta = &mut self.meta[entity.index as usize];
        debug_assert_eq!(meta.generation, entity.generation);
        meta.generation = meta.generation.wrapping_add(1);
        meta.location = EntityLocation::FREE;
        self.free.push(entity.index);
        self.alive -= 1;
    }

    pub(crate) fn location(&self, entity: Entity) -> Option<EntityLocation> {
        self.meta
            .get(entity.index as usize)
            .filter(|m| m.generation == entity.generation && m.location != EntityLocation::FREE)
            .map(|m| m.location)
    }

    pub(crate) fn set_location(&mut self, entity: Entity, location: EntityLocation) {
        self.meta[entity.index as usize].location = location;
    }

    pub(crate) fn alive(&self) -> u32 {
        self.alive
    }
}
