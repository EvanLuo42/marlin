//! The world: entities, their components, and resources.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::{Any, TypeId};
use core::cell::UnsafeCell;
use core::mem::{self, MaybeUninit};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use marlin_containers::{HashMap, HashMapExt};
use marlin_reflect::Reflect;
use marlin_std::thread::{ThreadId, current};

use crate::archetype::{Archetype, ArchetypeId, Archetypes};
use crate::bundle::{Bundle, BundleId, Bundles, Target};
use crate::change::{CHECK_TICK_THRESHOLD, ChangeTicks, Mut, Tick};
use crate::command::{CommandQueue, Commands};
use crate::component::{
    Component, ComponentDescriptor, ComponentHook, ComponentHooks, ComponentId, ComponentInfo,
    Components, HookContext, StorageType,
};
use crate::entity::{Entities, Entity, EntityLocation};
use crate::storage::{Column, ComponentTicks, SparseSets, Table, TableId, Tables};

type Resource = Box<UnsafeCell<dyn Any + Send + Sync>>;

/// A world's process-wide unique id, which [`QueryState`](crate::QueryState)s and
/// [`CommandQueue`]s check they are used with.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WorldId(u64);

static NEXT_WORLD_ID: AtomicU64 = AtomicU64::new(0);

/// Resources that are not `Send`/`Sync` (see [`World::insert_non_send`]): all of a world's live
/// on the thread that inserted the first one.
#[derive(Default)]
struct NonSendResources {
    map: HashMap<TypeId, Box<dyn Any>>,
    owner: Option<ThreadId>,
}

impl NonSendResources {
    #[track_caller]
    fn check_thread(&self) {
        if let Some(owner) = self.owner {
            assert!(
                owner == current::id(),
                "non-send resources were accessed from a thread other than the one that inserted them"
            );
        }
    }
}

impl Drop for NonSendResources {
    fn drop(&mut self) {
        // Dropping them elsewhere would break their `!Send`; leaking is safe.
        if self.owner.is_some_and(|owner| owner != current::id()) {
            mem::forget(mem::take(&mut self.map));
        }
    }
}

/// Which hook to run.
#[derive(Clone, Copy)]
enum HookKind {
    Add,
    Insert,
    Replace,
    Remove,
}

impl HookKind {
    fn get(self, hooks: &ComponentHooks) -> Option<ComponentHook> {
        match self {
            Self::Add => hooks.on_add,
            Self::Insert => hooks.on_insert,
            Self::Replace => hooks.on_replace,
            Self::Remove => hooks.on_remove,
        }
    }
}

pub struct World {
    id: WorldId,
    pub(crate) entities: Entities,
    pub(crate) components: Components,
    pub(crate) tables: Tables,
    pub(crate) archetypes: Archetypes,
    pub(crate) sparse_sets: SparseSets,
    pub(crate) bundles: Bundles,
    resources: HashMap<TypeId, Resource>,
    non_send: NonSendResources,
    /// Now, for change detection: writes are stamped with it. Systems advance it as they run.
    change_tick: AtomicU32,
    /// What queries through the world compare against (see [`clear_trackers`](Self::clear_trackers)).
    last_change_tick: Tick,
    /// When old ticks were last clamped.
    last_check_tick: Tick,
    /// Components removed (or despawned with their entity), by type, with the tick it happened at.
    removed: HashMap<ComponentId, Vec<(Entity, Tick)>>,
    /// Commands recorded by component hooks, applied when the outermost operation finishes.
    hook_queue: CommandQueue,
    pub(crate) applying_hooks: bool,
}

// SAFETY: every component and resource is Send + Sync. Shared access only reads, except through
// the scheduler's `SystemWorld`, which checks each system's writes against its declared access
// and runs only systems with disjoint access in parallel. Non-send resources are only handed out
// on their owner thread (checked on every access) and never dropped elsewhere.
unsafe impl Sync for World {}
// SAFETY: as above: moving the world moves non-send resources only as inert bytes.
unsafe impl Send for World {}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    pub fn new() -> Self {
        let mut tables = Tables::new();
        let archetypes = Archetypes::new(&mut tables);
        let mut world = Self {
            id: WorldId(NEXT_WORLD_ID.fetch_add(1, Ordering::Relaxed)),
            entities: Entities::default(),
            components: Components::new(),
            tables,
            archetypes,
            sparse_sets: SparseSets::default(),
            bundles: Bundles::new(),
            resources: HashMap::new(),
            non_send: NonSendResources::default(),
            // Tick 0 is everyone's first `last_run`: what exists at tick 1 reads as added.
            change_tick: AtomicU32::new(1),
            last_change_tick: Tick(0),
            last_check_tick: Tick(0),
            removed: HashMap::new(),
            hook_queue: CommandQueue::new(),
            applying_hooks: false,
        };
        crate::hierarchy::register(&mut world);
        world
    }

    #[inline]
    pub fn id(&self) -> WorldId {
        self.id
    }

    // --- Entities ---

    /// Makes ids reserved through [`Commands`] (or [`reserve_entity`](Self::reserve_entity)) live,
    /// component-less entities. Every structural operation does this first.
    pub fn flush(&mut self) {
        if !self.entities.needs_flush() {
            return;
        }
        let archetype = &mut self.archetypes.list[ArchetypeId::EMPTY.index()];
        let table = &mut self.tables.list[TableId::EMPTY.index()];
        self.entities.flush(|entity| {
            let table_row = table.entities.len() as u32;
            table.entities.push(entity);
            EntityLocation {
                archetype: ArchetypeId::EMPTY,
                archetype_row: archetype.push(entity, table_row),
                table: TableId::EMPTY,
                table_row,
            }
        });
    }

    /// An entity id usable right away, through a shared borrow; the entity becomes live (without
    /// components) at the next [`flush`](Self::flush).
    pub fn reserve_entity(&self) -> Entity {
        self.entities.reserve()
    }

    /// Spawns an entity with the components of `bundle`, a tuple such as `(Position, Velocity)`.
    ///
    /// # Panics
    /// If the bundle names a component type twice.
    pub fn spawn<B: Bundle>(&mut self, bundle: B) -> Entity {
        self.flush();
        let bundle_id = self.bundles.register::<B>(&mut self.components);
        let edge = self.bundle_edge(bundle_id, ArchetypeId::EMPTY);
        let entity = self.entities.alloc();
        let tick = self.change_tick();
        // SAFETY: the edge is the bundle's, from the empty archetype.
        unsafe { self.spawn_at(entity, edge, tick, |f| bundle.take(f)) };
        self.after_insert(entity, bundle_id, edge);
        entity
    }

    /// Spawns one entity per bundle, all into the same archetype, reserving storage up front from
    /// the iterator's size hint. Returns the entities in order.
    ///
    /// # Panics
    /// If the bundle names a component type twice.
    pub fn spawn_batch<I>(&mut self, bundles: I) -> Vec<Entity>
    where
        I: IntoIterator,
        I::Item: Bundle,
    {
        self.flush();
        let bundles = bundles.into_iter();
        let bundle_id = self.bundles.register::<I::Item>(&mut self.components);
        let edge = self.bundle_edge(bundle_id, ArchetypeId::EMPTY);
        let (additional, _) = bundles.size_hint();
        let target = self.bundles.edges[edge as usize].target;
        let archetype = &mut self.archetypes.list[target.index()];
        archetype.entities.reserve(additional);
        archetype.table_rows.reserve(additional);
        self.tables.list[archetype.table.index()].reserve(additional);
        self.entities.reserve_capacity(additional);
        let tick = self.change_tick();
        let mut spawned = Vec::with_capacity(additional);
        for bundle in bundles {
            let entity = self.entities.alloc();
            // SAFETY: as in `spawn`.
            unsafe { self.spawn_at(entity, edge, tick, |f| bundle.take(f)) };
            spawned.push(entity);
        }
        if self.bundle_has_hooks(bundle_id) {
            for &entity in &spawned {
                self.trigger_bundle(entity, bundle_id, edge, HookKind::Add, true);
                self.trigger_bundle(entity, bundle_id, edge, HookKind::Insert, false);
            }
        }
        self.drain_hooks();
        spawned
    }

    /// Spawns an entity without components.
    pub fn spawn_empty(&mut self) -> Entity {
        self.flush();
        let entity = self.entities.alloc();
        let archetype = &mut self.archetypes.list[ArchetypeId::EMPTY.index()];
        let table = &mut self.tables.list[TableId::EMPTY.index()];
        let table_row = table.entities.len() as u32;
        table.entities.push(entity);
        let archetype_row = archetype.push(entity, table_row);
        self.entities.set_location(
            entity,
            EntityLocation {
                archetype: ArchetypeId::EMPTY,
                archetype_row,
                table: TableId::EMPTY,
                table_row,
            },
        );
        entity
    }

    /// Removes the entity and drops its components. Returns false if it was not alive.
    pub fn despawn(&mut self, entity: Entity) -> bool {
        self.flush();
        let Some(location) = self.entities.location(entity) else {
            return false;
        };
        if self.archetype_has_hooks(location.archetype) {
            let ids = self.archetypes.list[location.archetype.index()]
                .components
                .to_vec();
            self.trigger(entity, &ids, HookKind::Replace);
            self.trigger(entity, &ids, HookKind::Remove);
        }
        // Hooks cannot change the world's structure, so the location still holds.
        let tick = self.change_tick();
        let archetype = &mut self.archetypes.list[location.archetype.index()];
        for &id in archetype.components.iter() {
            self.removed.entry(id).or_default().push((entity, tick));
        }
        for &id in archetype.sparse.iter() {
            let set = self.sparse_sets.get_mut(id).expect("sparse component set");
            set.remove_drop(entity);
        }
        swap_remove_archetype_row(
            &mut self.entities,
            archetype,
            location.archetype_row as usize,
        );
        let table = &mut self.tables.list[location.table.index()];
        let row = location.table_row as usize;
        for column in table.columns.iter_mut() {
            // SAFETY: `row` holds the entity's component.
            unsafe { column.swap_remove_drop(row) };
        }
        swap_remove_table_row(&mut self.entities, &mut self.archetypes, table, row);
        self.entities.free(entity);
        self.drain_hooks();
        true
    }

    #[inline]
    pub fn is_alive(&self, entity: Entity) -> bool {
        self.entities.location(entity).is_some()
    }

    /// Live entities (reserved ids count once flushed).
    #[inline]
    pub fn entity_count(&self) -> usize {
        self.entities.alive() as usize
    }

    /// The entity's component types, sorted by id.
    pub fn entity_components(&self, entity: Entity) -> Option<&[ComponentId]> {
        let location = self.entities.location(entity)?;
        Some(&self.archetypes.list[location.archetype.index()].components)
    }

    /// The archetype the entity is in.
    pub fn entity_archetype(&self, entity: Entity) -> Option<ArchetypeId> {
        self.entities.location(entity).map(|l| l.archetype)
    }

    // --- Components ---

    /// Adds `value` to the entity, replacing (and dropping) any existing `T`. Returns false if the
    /// entity is not alive.
    pub fn insert<T: Component>(&mut self, entity: Entity, value: T) -> bool {
        self.insert_bundle(entity, (value,))
    }

    /// Adds every component of `bundle` to the entity, replacing existing ones. Returns false (and
    /// drops the bundle) if the entity is not alive.
    ///
    /// # Panics
    /// If the bundle names a component type twice.
    pub fn insert_bundle<B: Bundle>(&mut self, entity: Entity, bundle: B) -> bool {
        let bundle_id = self.bundles.register::<B>(&mut self.components);
        // SAFETY: `take` hands over the bundle's components in its registration order.
        unsafe { self.insert_erased(entity, bundle_id, |f| bundle.take(f)) }
    }

    /// Inserts the components of `bundle` (registered as `bundle_id`), handed over by `take`.
    ///
    /// # Safety
    /// `take`, if called, passes `f(i, ptr)` once for each bundle component `i`, with a valid
    /// value of that component's type, which `f` takes ownership of.
    pub(crate) unsafe fn insert_erased(
        &mut self,
        entity: Entity,
        bundle_id: BundleId,
        take: impl FnOnce(&mut dyn FnMut(usize, *mut u8)),
    ) -> bool {
        self.flush();
        let Some(location) = self.entities.location(entity) else {
            return false;
        };
        let edge = self.bundle_edge(bundle_id, location.archetype);
        let (target, replaces) = {
            let edge = &self.bundles.edges[edge as usize];
            (edge.target, edge.added.iter().any(|&added| !added))
        };
        if replaces && self.bundle_has_hooks(bundle_id) {
            self.trigger_bundle(entity, bundle_id, edge, HookKind::Replace, false);
        }
        let location = if target == location.archetype {
            location
        } else {
            self.move_entity(entity, location, target, |_, _, _| {
                unreachable!("adding components keeps every column")
            })
        };
        let tick = self.change_tick();
        // SAFETY: the location is in the edge's target; forwarded from the caller.
        unsafe { self.write_bundle(entity, location, edge, tick, take) };
        self.after_insert(entity, bundle_id, edge);
        true
    }

    /// Removes and returns the entity's `T`, if it is alive and has one.
    pub fn remove<T: Component>(&mut self, entity: Entity) -> Option<T> {
        let id = self.components.id_of::<T>()?;
        let mut out = MaybeUninit::<T>::uninit();
        // SAFETY: `id` is `T`'s, so `out` is storage for the value.
        let taken = unsafe { self.take_by_id(entity, id, Some(out.as_mut_ptr().cast())) };
        // SAFETY: written by `take_by_id` when it returns true.
        taken.then(|| unsafe { out.assume_init() })
    }

    /// Removes component `id` from the entity, moving its value to `dst` (or dropping it). False
    /// if the entity is dead or lacks the component.
    ///
    /// # Safety
    /// `dst`, if any, is writable storage for one value of the component.
    pub(crate) unsafe fn take_by_id(
        &mut self,
        entity: Entity,
        id: ComponentId,
        dst: Option<*mut u8>,
    ) -> bool {
        self.flush();
        let Some(location) = self.entities.location(entity) else {
            return false;
        };
        if !self.archetypes.list[location.archetype.index()].contains(id) {
            return false;
        }
        let hooks = self.components.info(id).hooks;
        if hooks.on_replace.is_some() || hooks.on_remove.is_some() {
            self.trigger(entity, &[id], HookKind::Replace);
            self.trigger(entity, &[id], HookKind::Remove);
        }
        let target = self.archetypes.with_removed(
            location.archetype,
            id,
            &self.components,
            &mut self.tables,
        );
        let tick = self.change_tick();
        self.removed.entry(id).or_default().push((entity, tick));
        match self.components.info(id).storage() {
            StorageType::SparseSet => {
                let set = self.sparse_sets.get_mut(id).expect("sparse component set");
                match dst {
                    // SAFETY: forwarded from the caller.
                    Some(dst) => unsafe { set.remove_into(entity, dst) },
                    None => set.remove_drop(entity),
                };
                self.move_entity(entity, location, target, |_, _, _| {
                    unreachable!("removing a sparse component keeps the table")
                });
            }
            StorageType::Table => {
                self.move_entity(entity, location, target, |c, column, row| {
                    debug_assert_eq!(c, id);
                    // SAFETY: the only column missing from the target is the removed one; its
                    // value is moved out (forwarded from the caller) or dropped.
                    unsafe {
                        match dst {
                            Some(dst) => column.swap_remove_into(row, dst),
                            None => column.swap_remove_drop(row),
                        }
                    }
                });
            }
        }
        self.drain_hooks();
        true
    }

    pub fn get<T: Component>(&self, entity: Entity) -> Option<&T> {
        let id = self.components.id_of::<T>()?;
        // SAFETY: shared access to the world; the cell holds a `T`.
        self.component_cells(entity, id)
            .map(|(p, _)| unsafe { &*p.cast::<T>() })
    }

    /// The entity's `T` for writing; writes through the [`Mut`] mark it changed.
    pub fn get_mut<T: Component>(&mut self, entity: Entity) -> Option<Mut<'_, T>> {
        let change = ChangeTicks {
            last_run: self.last_change_tick,
            this_run: self.change_tick(),
        };
        let id = self.components.id_of::<T>()?;
        let (value, ticks) = self.component_cells(entity, id)?;
        // SAFETY: exclusive access to the world; the cell holds a `T`, and the ticks are a
        // separate allocation.
        Some(unsafe { Mut::new(&mut *value.cast::<T>(), &mut *ticks, change) })
    }

    /// When the entity's `T` was added and last changed.
    pub fn component_ticks<T: Component>(&self, entity: Entity) -> Option<ComponentTicks> {
        let id = self.components.id_of::<T>()?;
        // SAFETY: shared access; the ticks are plain data.
        self.component_cells(entity, id).map(|(_, t)| unsafe { *t })
    }

    #[inline]
    pub fn has<T: Component>(&self, entity: Entity) -> bool {
        self.components
            .id_of::<T>()
            .is_some_and(|id| self.has_id(entity, id))
    }

    #[inline]
    pub fn has_id(&self, entity: Entity, id: ComponentId) -> bool {
        self.entities
            .location(entity)
            .is_some_and(|l| self.archetypes.list[l.archetype.index()].contains(id))
    }

    /// The entity's component `id` and its ticks, as pointers for callers that uphold the access
    /// rules.
    #[inline]
    pub(crate) fn component_cells(
        &self,
        entity: Entity,
        id: ComponentId,
    ) -> Option<(*mut u8, *mut ComponentTicks)> {
        let location = self.entities.location(entity)?;
        match self.components.get(id)?.storage() {
            StorageType::Table => {
                let column = self.tables.list[location.table.index()].column(id)?;
                let row = location.table_row as usize;
                // SAFETY: `row` is in bounds; the tick cell gives a raw pointer without a
                // reference.
                Some((column.ptr(row), unsafe {
                    (*column.ticks_ptr().add(row)).get()
                }))
            }
            StorageType::SparseSet => {
                let set = self.sparse_sets.get(id)?;
                let row = set.row(entity)?;
                // SAFETY: as above.
                Some((set.dense.ptr(row), unsafe {
                    (*set.dense.ticks_ptr().add(row)).get()
                }))
            }
        }
    }

    /// Registers `T` now rather than on first use.
    pub fn register_component<T: Component>(&mut self) -> ComponentId {
        self.components.register::<T>()
    }

    /// Registers `T` with a storage type; see [`StorageType`].
    ///
    /// # Panics
    /// If `T` is already registered (for example by an earlier spawn or query) with another
    /// storage type.
    pub fn register_component_with<T: Component>(&mut self, storage: StorageType) -> ComponentId {
        self.components
            .register_descriptor(ComponentDescriptor::new::<T>().with_storage(storage))
    }

    /// Registers a component type from its description, for components the VM or the editor
    /// define. A descriptor with a Rust type shares that type's id.
    ///
    /// # Panics
    /// As for [`register_component_with`](Self::register_component_with).
    pub fn register_dynamic(&mut self, descriptor: ComponentDescriptor) -> ComponentId {
        self.components.register_descriptor(descriptor)
    }

    /// Registers `T` with its reflected description, for the editor and the VM; it also becomes
    /// cloneable through reflection if `T` reflects `Clone`.
    pub fn register_reflected<T: Component + Reflect>(&mut self) -> ComponentId {
        self.components.register_reflected::<T>()
    }

    /// Lets [`clone_entity`](Self::clone_entity) copy `T`.
    pub fn register_cloneable<T: Component + Clone>(&mut self) -> ComponentId {
        self.components.register_clone::<T>()
    }

    /// The hooks of `T`, registering it if needed; see [`ComponentHooks`].
    pub fn component_hooks<T: Component>(&mut self) -> &mut ComponentHooks {
        let id = self.components.register::<T>();
        self.components.hooks_mut(id)
    }

    /// The hooks of a component by id.
    pub fn component_hooks_by_id(&mut self, id: ComponentId) -> &mut ComponentHooks {
        self.components.hooks_mut(id)
    }

    pub fn component_id<T: Component>(&self) -> Option<ComponentId> {
        self.components.id_of::<T>()
    }

    /// The component registered for a Rust type.
    pub fn component_id_of_type(&self, type_id: TypeId) -> Option<ComponentId> {
        self.components.id_of_type(type_id)
    }

    /// # Panics
    /// If `id` is not a component of this world.
    pub fn component_info(&self, id: ComponentId) -> &ComponentInfo {
        self.components.info(id)
    }

    pub fn components(&self) -> impl Iterator<Item = (ComponentId, &ComponentInfo)> {
        self.components.iter()
    }

    pub fn archetypes(&self) -> &[Archetype] {
        &self.archetypes.list
    }

    pub fn tables(&self) -> &[Table] {
        &self.tables.list
    }

    #[inline]
    fn bundle_edge(&mut self, bundle: BundleId, from: ArchetypeId) -> u32 {
        self.bundles.edge(
            bundle,
            from,
            &mut self.archetypes,
            &self.components,
            &mut self.tables,
        )
    }

    /// Places a fresh `entity` in the edge's target archetype and writes the bundle there.
    ///
    /// # Safety
    /// `edge` leads from the empty archetype; `take` as for
    /// [`insert_erased`](Self::insert_erased).
    #[inline]
    unsafe fn spawn_at(
        &mut self,
        entity: Entity,
        edge: u32,
        tick: Tick,
        take: impl FnOnce(&mut dyn FnMut(usize, *mut u8)),
    ) {
        let target = self.bundles.edges[edge as usize].target;
        let archetype = &mut self.archetypes.list[target.index()];
        let table = &mut self.tables.list[archetype.table.index()];
        let table_row = table.entities.len() as u32;
        table.entities.push(entity);
        let location = EntityLocation {
            archetype: target,
            archetype_row: archetype.push(entity, table_row),
            table: archetype.table,
            table_row,
        };
        self.entities.set_location(entity, location);
        // SAFETY: forwarded from the caller.
        unsafe { self.write_bundle(entity, location, edge, tick, take) };
    }

    /// Writes the components `take` hands over into the entity's row in the edge's target
    /// archetype: pushing added ones, replacing the others.
    ///
    /// # Safety
    /// The entity is at `location`, in the edge's target, with every added table column one row
    /// short; `take` as for [`insert_erased`](Self::insert_erased).
    #[inline]
    unsafe fn write_bundle(
        &mut self,
        entity: Entity,
        location: EntityLocation,
        edge: u32,
        tick: Tick,
        take: impl FnOnce(&mut dyn FnMut(usize, *mut u8)),
    ) {
        let edge = &self.bundles.edges[edge as usize];
        let table = &mut self.tables.list[location.table.index()];
        let sparse_sets = &mut self.sparse_sets;
        let components = &self.components;
        let row = location.table_row as usize;
        let ticks = ComponentTicks::new(tick);
        let mut put = |i: usize, ptr: *mut u8| {
            // SAFETY: `ptr` is a valid value of the `i`-th component, ours to move from (the
            // caller); the targets were computed for this table.
            unsafe {
                match (edge.targets[i], edge.added[i]) {
                    (Target::Column(c), true) => table.columns[c as usize].push(ptr, ticks),
                    (Target::Column(c), false) => table.columns[c as usize].replace(row, ptr, tick),
                    (Target::Sparse(id), true) => sparse_sets
                        .get_or_create(components.info(id))
                        .insert_new(entity, ptr, ticks),
                    (Target::Sparse(id), false) => sparse_sets
                        .get_mut(id)
                        .expect("sparse component set")
                        .replace(entity, ptr, tick),
                }
            }
        };
        take(&mut put);
    }

    /// Hooks after a bundle was written.
    #[inline]
    fn after_insert(&mut self, entity: Entity, bundle: BundleId, edge: u32) {
        if self.bundle_has_hooks(bundle) {
            self.trigger_bundle(entity, bundle, edge, HookKind::Add, true);
            self.trigger_bundle(entity, bundle, edge, HookKind::Insert, false);
            self.drain_hooks();
        }
    }

    /// Moves `entity` from `from` to archetype `to`. Table components both share move along; for
    /// each one the target table lacks, `leftover(id, column, row)` must move it out or drop it
    /// and remove the row. Sparse-set components stay where they are: they are the caller's to
    /// add or remove, as are table components the target adds. Returns the new location.
    fn move_entity(
        &mut self,
        entity: Entity,
        from: EntityLocation,
        to: ArchetypeId,
        mut leftover: impl FnMut(ComponentId, &mut Column, usize),
    ) -> EntityLocation {
        debug_assert_ne!(from.archetype, to);
        let to_table = self.archetypes.list[to.index()].table;
        swap_remove_archetype_row(
            &mut self.entities,
            &mut self.archetypes.list[from.archetype.index()],
            from.archetype_row as usize,
        );
        let table_row = if to_table == from.table {
            from.table_row
        } else {
            let (source, target) = self.tables.pair_mut(from.table, to_table);
            let row = from.table_row as usize;
            for i in 0..source.components.len() {
                let id = source.components[i];
                match target.column_index(id) {
                    Some(j) => {
                        // SAFETY: the row's item moves to the target, then leaves the source
                        // without being dropped.
                        unsafe {
                            let ticks = source.columns[i].ticks(row);
                            target.columns[j].push(source.columns[i].ptr(row), ticks);
                            source.columns[i].swap_remove_forget(row);
                        }
                    }
                    None => leftover(id, &mut source.columns[i], row),
                }
            }
            let new_row = target.entities.len() as u32;
            target.entities.push(entity);
            swap_remove_table_row(&mut self.entities, &mut self.archetypes, source, row);
            new_row
        };
        let location = EntityLocation {
            archetype: to,
            archetype_row: self.archetypes.list[to.index()].push(entity, table_row),
            table: to_table,
            table_row,
        };
        self.entities.set_location(entity, location);
        location
    }

    // --- Hooks ---

    fn bundle_has_hooks(&self, bundle: BundleId) -> bool {
        self.bundles.components(bundle).iter().any(|&id| {
            let hooks = &self.components.info(id).hooks;
            hooks.on_add.is_some() || hooks.on_insert.is_some() || hooks.on_replace.is_some()
        })
    }

    fn archetype_has_hooks(&self, archetype: ArchetypeId) -> bool {
        self.archetypes.list[archetype.index()]
            .components
            .iter()
            .any(|&id| {
                let hooks = &self.components.info(id).hooks;
                hooks.on_replace.is_some() || hooks.on_remove.is_some()
            })
    }

    /// Runs `kind` hooks for the bundle's components: those it added if `added_only`, else all
    /// (`Replace` takes the ones it did not add).
    fn trigger_bundle(
        &mut self,
        entity: Entity,
        bundle: BundleId,
        edge: u32,
        kind: HookKind,
        added_only: bool,
    ) {
        let count = self.bundles.components(bundle).len();
        for i in 0..count {
            let added = self.bundles.edges[edge as usize].added[i];
            let wanted = match kind {
                HookKind::Replace => !added,
                _ => added || !added_only,
            };
            if wanted {
                let id = self.bundles.components(bundle)[i];
                self.trigger(entity, &[id], kind);
            }
        }
    }

    fn trigger(&mut self, entity: Entity, ids: &[ComponentId], kind: HookKind) {
        for &id in ids {
            if let Some(hook) = kind.get(&self.components.info(id).hooks) {
                let mut queue = mem::take(&mut self.hook_queue);
                {
                    let world: &World = self;
                    let mut context = HookContext {
                        world,
                        entity,
                        component: id,
                        commands: Commands::from_entities(&mut queue, &world.entities, world.id),
                    };
                    hook(&mut context);
                }
                self.hook_queue = queue;
            }
        }
    }

    /// Applies the commands hooks recorded, unless an outer call is already doing so.
    pub(crate) fn drain_hooks(&mut self) {
        if self.applying_hooks || self.hook_queue.is_empty() {
            return;
        }
        self.applying_hooks = true;
        loop {
            let mut queue = mem::take(&mut self.hook_queue);
            if queue.is_empty() {
                break;
            }
            self.apply(&mut queue);
        }
        self.applying_hooks = false;
    }

    // --- Change detection ---

    /// Now: the tick writes are stamped with.
    #[inline]
    pub fn change_tick(&self) -> Tick {
        Tick(self.change_tick.load(Ordering::Acquire))
    }

    /// Advances the change tick, returning the tick before. Each system run takes one.
    #[inline]
    pub fn increment_change_tick(&self) -> Tick {
        Tick(self.change_tick.fetch_add(1, Ordering::AcqRel))
    }

    /// What queries through the world (rather than a system) compare against: they report
    /// changes made since this tick.
    #[inline]
    pub fn last_change_tick(&self) -> Tick {
        self.last_change_tick
    }

    /// Ends a change-detection period, once a frame: queries through the world stop reporting
    /// what happened so far, removal records older than the previous period are dropped, and old
    /// ticks are clamped when due. Systems keep their own `last_run` and are unaffected.
    pub fn clear_trackers(&mut self) {
        let previous = self.last_change_tick;
        let now = self.increment_change_tick();
        for entries in self.removed.values_mut() {
            // Removals of the period now ending are kept for one more, so a reader that runs once
            // per period still sees them; those of the period before (at or before `previous`)
            // go.
            entries.retain(|&(_, tick)| tick.is_newer_than(previous, now));
        }
        self.last_change_tick = now;
        self.check_change_ticks();
    }

    /// Clamps ticks older than [`MAX_CHANGE_AGE`](crate::change::MAX_CHANGE_AGE) if
    /// [`CHECK_TICK_THRESHOLD`] ticks have passed since the last check, returning the tick
    /// checked against. Readers with their own `last_run` (systems) clamp it with the result.
    pub fn check_change_ticks(&mut self) -> Option<Tick> {
        let now = self.change_tick();
        if now.0.wrapping_sub(self.last_check_tick.0) < CHECK_TICK_THRESHOLD {
            return None;
        }
        for table in &mut self.tables.list {
            for column in table.columns.iter_mut() {
                column.check_ticks(now);
            }
        }
        for set in self.sparse_sets.iter_mut() {
            set.dense.check_ticks(now);
        }
        for entries in self.removed.values_mut() {
            for (_, tick) in entries.iter_mut() {
                tick.clamp(now);
            }
        }
        self.last_change_tick.clamp(now);
        self.last_check_tick = now;
        Some(now)
    }

    /// Entities whose `T` was removed (or that were despawned with one) after `since`, as seen at
    /// `now`. Records last until two [`clear_trackers`](Self::clear_trackers) later.
    pub fn removed_since<T: Component>(
        &self,
        since: Tick,
        now: Tick,
    ) -> impl Iterator<Item = Entity> + '_ {
        self.component_id::<T>()
            .and_then(|id| self.removed.get(&id))
            .into_iter()
            .flatten()
            .filter(move |&&(_, tick)| tick.is_newer_than(since, now))
            .map(|&(entity, _)| entity)
    }

    /// [`removed_since`](Self::removed_since) the world's
    /// [`last_change_tick`](Self::last_change_tick).
    pub fn removed<T: Component>(&self) -> impl Iterator<Item = Entity> + '_ {
        self.removed_since::<T>(self.last_change_tick, self.change_tick())
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

    /// The resource, inserting `R::default()` first if there is none.
    pub fn resource_or_default<R: Send + Sync + Default + 'static>(&mut self) -> &mut R {
        if !self.has_resource::<R>() {
            self.insert_resource(R::default());
        }
        self.resource_mut::<R>().expect("just inserted")
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

    // --- Non-send resources ---

    /// Stores a singleton that need not be `Send` or `Sync`, such as a window handle or an `Rc`.
    ///
    /// Non-send resources belong to the thread that inserted the first one (normally the main
    /// thread, which calls [`Schedule::run`](crate::Schedule::run)); every access from another
    /// thread panics. Shared systems run on worker threads, so use them from exclusive systems,
    /// which run on the thread calling `run`, or outside the schedule. If the world is dropped on
    /// another thread, they are leaked rather than dropped there.
    ///
    /// # Panics
    /// On a thread other than the owner.
    #[track_caller]
    pub fn insert_non_send<R: 'static>(&mut self, resource: R) {
        self.non_send.check_thread();
        self.non_send.owner = Some(current::id());
        self.non_send
            .map
            .insert(TypeId::of::<R>(), Box::new(resource));
    }

    /// # Panics
    /// On a thread other than the owner.
    #[track_caller]
    pub fn remove_non_send<R: 'static>(&mut self) -> Option<R> {
        self.non_send.check_thread();
        let value = self.non_send.map.remove(&TypeId::of::<R>())?;
        value.downcast::<R>().ok().map(|b| *b)
    }

    /// # Panics
    /// On a thread other than the owner.
    #[track_caller]
    pub fn non_send<R: 'static>(&self) -> Option<&R> {
        self.non_send.check_thread();
        self.non_send.map.get(&TypeId::of::<R>())?.downcast_ref()
    }

    /// # Panics
    /// On a thread other than the owner.
    #[track_caller]
    pub fn non_send_mut<R: 'static>(&mut self) -> Option<&mut R> {
        self.non_send.check_thread();
        self.non_send
            .map
            .get_mut(&TypeId::of::<R>())?
            .downcast_mut()
    }

    pub fn has_non_send<R: 'static>(&self) -> bool {
        self.non_send.map.contains_key(&TypeId::of::<R>())
    }
}

/// Swap-removes `row` from the archetype's entity list, fixing the location of the entity moved
/// into it.
fn swap_remove_archetype_row(entities: &mut Entities, archetype: &mut Archetype, row: usize) {
    archetype.entities.swap_remove(row);
    archetype.table_rows.swap_remove(row);
    if let Some(&moved) = archetype.entities.get(row) {
        entities.location_mut(moved).archetype_row = row as u32;
    }
}

/// Swap-removes `row` from the table's entity list, fixing the location (and archetype row
/// entry) of the entity moved into it. The columns must already have been swap-removed the same
/// way.
fn swap_remove_table_row(
    entities: &mut Entities,
    archetypes: &mut Archetypes,
    table: &mut Table,
    row: usize,
) {
    table.entities.swap_remove(row);
    if let Some(&moved) = table.entities.get(row) {
        let location = entities.location_mut(moved);
        location.table_row = row as u32;
        archetypes.list[location.archetype.index()].table_rows[location.archetype_row as usize] =
            row as u32;
    }
}
