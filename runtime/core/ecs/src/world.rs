//! The world: entities, their components, and resources.

use alloc::boxed::Box;
use core::any::{Any, TypeId};
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;

use marlin_containers::{HashMap, HashMapExt};
use marlin_reflect::Reflect;

use crate::archetype::{Archetype, ArchetypeId, Archetypes};
use crate::bundle::{Bundle, MAX_BUNDLE};
use crate::component::{Component, ComponentId, ComponentInfo, Components};
use crate::entity::{Entities, Entity, EntityLocation};
use crate::storage::Column;

type Resource = Box<UnsafeCell<dyn Any + Send + Sync>>;

pub struct World {
    pub(crate) entities: Entities,
    pub(crate) components: Components,
    pub(crate) archetypes: Archetypes,
    resources: HashMap<TypeId, Resource>,
}

// SAFETY: every component and resource is Send + Sync. Shared access only reads, except through
// the scheduler's `SystemWorld`, which checks each system's writes against its declared access
// and runs only systems with disjoint access in parallel.
unsafe impl Sync for World {}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    pub fn new() -> Self {
        Self {
            entities: Entities::default(),
            components: Components::new(),
            archetypes: Archetypes::new(),
            resources: HashMap::new(),
        }
    }

    // --- Entities ---

    /// Spawns an entity with the components of `bundle`, a tuple such as `(Position, Velocity)`.
    ///
    /// # Panics
    /// If the bundle names a component type twice.
    pub fn spawn<B: Bundle>(&mut self, bundle: B) -> Entity {
        const { assert!(B::COUNT <= MAX_BUNDLE) };
        let mut ids = [ComponentId(0); MAX_BUNDLE];
        B::register(&mut self.components, &mut ids[..B::COUNT]);
        let mut sorted = ids;
        let sorted = &mut sorted[..B::COUNT];
        sorted.sort_unstable();
        assert!(
            sorted.windows(2).all(|w| w[0] != w[1]),
            "a bundle names a component type twice"
        );
        let archetype_id = self.archetypes.get_or_create(sorted, &self.components);

        let entity = self.entities.alloc();
        let archetype = &mut self.archetypes.list[archetype_id.index()];
        let row = archetype.len();
        let mut put = |i: usize, ptr: *mut u8| {
            let column = archetype.column_index(ids[i]).expect("bundle column");
            // SAFETY: `take` hands over a valid component of this column's type.
            unsafe { archetype.columns[column].push(ptr) };
        };
        // SAFETY: `put` takes ownership of every component.
        unsafe { bundle.take(&mut put) };
        archetype.entities.push(entity);
        self.entities.set_location(
            entity,
            EntityLocation {
                archetype: archetype_id,
                row: row as u32,
            },
        );
        entity
    }

    /// Spawns an entity without components.
    pub fn spawn_empty(&mut self) -> Entity {
        let entity = self.entities.alloc();
        let archetype = &mut self.archetypes.list[ArchetypeId::EMPTY.index()];
        let row = archetype.len() as u32;
        archetype.entities.push(entity);
        self.entities.set_location(
            entity,
            EntityLocation {
                archetype: ArchetypeId::EMPTY,
                row,
            },
        );
        entity
    }

    /// Removes the entity and drops its components. Returns false if it was not alive.
    pub fn despawn(&mut self, entity: Entity) -> bool {
        let Some(location) = self.entities.location(entity) else {
            return false;
        };
        let archetype = &mut self.archetypes.list[location.archetype.index()];
        let row = location.row as usize;
        for column in archetype.columns.iter_mut() {
            // SAFETY: `row` holds the entity's component.
            unsafe { column.swap_remove_drop(row) };
        }
        Self::remove_row(&mut self.entities, archetype, row);
        self.entities.free(entity);
        true
    }

    #[inline]
    pub fn is_alive(&self, entity: Entity) -> bool {
        self.entities.location(entity).is_some()
    }

    /// Live entities.
    #[inline]
    pub fn entity_count(&self) -> usize {
        self.entities.alive() as usize
    }

    // --- Components ---

    /// Adds `value` to the entity, replacing (and dropping) any existing `T`. Returns false if the
    /// entity is not alive.
    pub fn insert<T: Component>(&mut self, entity: Entity, value: T) -> bool {
        let Some(location) = self.entities.location(entity) else {
            return false;
        };
        let id = self.components.register::<T>();
        let mut value = MaybeUninit::new(value);
        let source = &mut self.archetypes.list[location.archetype.index()];
        if let Some(column) = source.column_index(id) {
            // SAFETY: the row holds a `T`; `value` is moved in and never used again.
            unsafe { source.columns[column].replace(location.row as usize, value.as_ptr().cast()) };
            return true;
        }
        let target = self
            .archetypes
            .with_added(location.archetype, id, &self.components);
        let row = self.transfer(entity, location, target, |_, _| {
            unreachable!("adding a component keeps every column")
        });
        let target = &mut self.archetypes.list[target.index()];
        let column = target.column_index(id).expect("added column");
        // SAFETY: `value` is moved in and never used again.
        unsafe { target.columns[column].push(value.as_mut_ptr().cast()) };
        debug_assert_eq!(target.columns[column].len(), row + 1);
        true
    }

    /// Removes and returns the entity's `T`, if it is alive and has one.
    pub fn remove<T: Component>(&mut self, entity: Entity) -> Option<T> {
        let location = self.entities.location(entity)?;
        let id = self.components.id_of::<T>()?;
        if !self.archetypes.list[location.archetype.index()].contains(id) {
            return None;
        }
        let target = self
            .archetypes
            .with_removed(location.archetype, id, &self.components);
        let mut out = MaybeUninit::<T>::uninit();
        self.transfer(entity, location, target, |column, row| {
            // SAFETY: the only column missing from the target is `T`'s; the value is moved out
            // and its row then removed without dropping.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    column.ptr(row),
                    out.as_mut_ptr().cast(),
                    size_of::<T>(),
                );
                column.swap_remove_forget(row);
            }
        });
        // SAFETY: written by the closure above.
        Some(unsafe { out.assume_init() })
    }

    pub fn get<T: Component>(&self, entity: Entity) -> Option<&T> {
        // SAFETY: shared access to the world.
        self.component_ptr::<T>(entity).map(|p| unsafe { &*p })
    }

    pub fn get_mut<T: Component>(&mut self, entity: Entity) -> Option<&mut T> {
        // SAFETY: exclusive access to the world.
        self.component_ptr::<T>(entity).map(|p| unsafe { &mut *p })
    }

    #[inline]
    pub fn has<T: Component>(&self, entity: Entity) -> bool {
        self.component_ptr::<T>(entity).is_some()
    }

    pub(crate) fn component_ptr<T: Component>(&self, entity: Entity) -> Option<*mut T> {
        let location = self.entities.location(entity)?;
        let id = self.components.id_of::<T>()?;
        let column = self.archetypes.list[location.archetype.index()].column(id)?;
        Some(column.ptr(location.row as usize).cast())
    }

    /// Registers `T` now rather than on first use.
    pub fn register_component<T: Component>(&mut self) -> ComponentId {
        self.components.register::<T>()
    }

    /// Registers `T` with its reflected description, for the editor and the VM.
    pub fn register_reflected<T: Component + Reflect>(&mut self) -> ComponentId {
        self.components.register_reflected::<T>()
    }

    pub fn component_id<T: Component>(&self) -> Option<ComponentId> {
        self.components.id_of::<T>()
    }

    pub fn component_info(&self, id: ComponentId) -> &ComponentInfo {
        self.components.info(id)
    }

    pub fn components(&self) -> impl Iterator<Item = (ComponentId, &ComponentInfo)> {
        self.components.iter()
    }

    pub fn archetypes(&self) -> &[Archetype] {
        &self.archetypes.list
    }

    /// Moves `entity` from `location` to archetype `to`, carrying every component both share.
    /// For each source component `to` lacks, `leftover(column, row)` must move it out or drop it
    /// and remove the row. Returns the entity's new row (components `to` adds are the caller's to
    /// push).
    fn transfer(
        &mut self,
        entity: Entity,
        location: EntityLocation,
        to: ArchetypeId,
        mut leftover: impl FnMut(&mut Column, usize),
    ) -> usize {
        let (source, target) = self.archetypes.pair_mut(location.archetype, to);
        let row = location.row as usize;
        for i in 0..source.components.len() {
            let id = source.components[i];
            match target.column_index(id) {
                Some(j) => {
                    // SAFETY: the row's item moves to the target, then leaves the source
                    // without being dropped.
                    unsafe {
                        target.columns[j].push(source.columns[i].ptr(row));
                        source.columns[i].swap_remove_forget(row);
                    }
                }
                None => leftover(&mut source.columns[i], row),
            }
        }
        let new_row = target.len();
        target.entities.push(entity);
        Self::remove_row(&mut self.entities, source, row);
        self.entities.set_location(
            entity,
            EntityLocation {
                archetype: to,
                row: new_row as u32,
            },
        );
        new_row
    }

    /// Swap-removes `row` from the archetype's entity list, fixing the location of the entity
    /// moved into it. The columns must already have been swap-removed the same way.
    fn remove_row(entities: &mut Entities, archetype: &mut Archetype, row: usize) {
        archetype.entities.swap_remove(row);
        if let Some(&moved) = archetype.entities.get(row) {
            entities.set_location(
                moved,
                EntityLocation {
                    archetype: archetype.id(),
                    row: row as u32,
                },
            );
        }
    }

    // --- Resources ---

    /// Stores a singleton, replacing any previous `R`.
    pub fn insert_resource<R: Send + Sync + 'static>(&mut self, resource: R) {
        let cell: Resource = Box::new(UnsafeCell::new(resource));
        self.resources.insert(TypeId::of::<R>(), cell);
    }

    pub fn remove_resource<R: Send + Sync + 'static>(&mut self) -> Option<R> {
        let cell = self.resources.remove(&TypeId::of::<R>())?;
        let raw = Box::into_raw(cell).cast::<UnsafeCell<R>>();
        // SAFETY: entries are keyed by `TypeId::of::<R>()`, so this one was a `Box<UnsafeCell<R>>`.
        Some(unsafe { Box::from_raw(raw) }.into_inner())
    }

    pub fn resource<R: Send + Sync + 'static>(&self) -> Option<&R> {
        // SAFETY: shared access; writers go through `&mut World` or the scheduler's checks.
        self.resource_ptr::<R>().map(|p| unsafe { &*p })
    }

    pub fn resource_mut<R: Send + Sync + 'static>(&mut self) -> Option<&mut R> {
        // SAFETY: exclusive access to the world.
        self.resource_ptr::<R>().map(|p| unsafe { &mut *p })
    }

    pub fn has_resource<R: Send + Sync + 'static>(&self) -> bool {
        self.resources.contains_key(&TypeId::of::<R>())
    }

    /// A pointer to the resource; no reference to it is formed, so concurrent users of other
    /// resources are unaffected.
    pub(crate) fn resource_ptr<R: Send + Sync + 'static>(&self) -> Option<*mut R> {
        // Entries are keyed by `TypeId::of::<R>()`, so the cell holds an `R`.
        let cell = self.resources.get(&TypeId::of::<R>())?;
        Some(cell.get().cast::<R>())
    }
}
