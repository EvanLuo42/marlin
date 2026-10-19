//! Access by [`ComponentId`] for code that knows components only as bytes and reflected types:
//! the VM and the editor. Every path here is the typed path minus the type: the same storage,
//! change ticks, hooks and access checks.

use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use alloc::vec::Vec;
use core::alloc::Layout;
use core::ptr::{self, NonNull};

use marlin_reflect::Value;

use crate::archetype::ArchetypeId;
use crate::change::{ChangeTicks, Tick, TickCell};
use crate::component::{ComponentId, ComponentInfo};
use crate::entity::Entity;
use crate::hierarchy::{Children, Parent};
use crate::query::{Access, AccessKey};
use crate::storage::{ComponentTicks, SparseSet};
use crate::world::{World, WorldId};

/// A component of an entity, read through its id.
pub struct ComponentPtr<'w> {
    ptr: *const u8,
    info: &'w ComponentInfo,
}

impl<'w> ComponentPtr<'w> {
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    pub fn info(&self) -> &'w ComponentInfo {
        self.info
    }

    /// The value through its reflected description, if registered with one.
    pub fn reflect(&self) -> Option<marlin_reflect::Ref<'w>> {
        // SAFETY: the pointer is a live value of the reflected type, shared for `'w`.
        self.info
            .reflect()
            .map(|r| unsafe { marlin_reflect::Ref::from_raw(self.ptr, r) })
    }

    /// # Safety
    /// The component's type is `T`.
    pub unsafe fn deref<T>(self) -> &'w T {
        // SAFETY: forwarded from the caller; shared for `'w`.
        unsafe { &*self.ptr.cast::<T>() }
    }
}

/// A component of an entity, written through its id: getting a mutable pointer marks it changed.
pub struct ComponentMut<'w> {
    ptr: *mut u8,
    info: &'w ComponentInfo,
    ticks: &'w mut ComponentTicks,
    this_run: Tick,
}

impl<'w> ComponentMut<'w> {
    /// # Safety
    /// `ptr` is a live value of `info`'s component with `ticks` its row's ticks, both exclusive
    /// for `'w`.
    pub(crate) unsafe fn from_raw(
        ptr: *mut u8,
        info: &'w ComponentInfo,
        ticks: &'w mut ComponentTicks,
        this_run: Tick,
    ) -> Self {
        Self {
            ptr,
            info,
            ticks,
            this_run,
        }
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    /// The value for writing, marked changed.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ticks.changed = self.this_run;
        self.ptr
    }

    pub fn info(&self) -> &'w ComponentInfo {
        self.info
    }

    pub fn ticks(&self) -> ComponentTicks {
        *self.ticks
    }

    pub fn reflect(&self) -> Option<marlin_reflect::Ref<'_>> {
        // SAFETY: a live value of the reflected type, shared for the borrow of `self`.
        self.info
            .reflect()
            .map(|r| unsafe { marlin_reflect::Ref::from_raw(self.ptr, r) })
    }

    /// The value through its reflected description for writing, marked changed.
    pub fn reflect_mut(&mut self) -> Option<marlin_reflect::Mut<'_>> {
        let info = self.info.reflect()?;
        let ptr = self.as_mut_ptr();
        // SAFETY: a live value of the reflected type, exclusive for the borrow of `self`.
        Some(unsafe { marlin_reflect::Mut::from_raw(ptr, info) })
    }
}

/// One value's storage in [`ErasedValues`].
struct ErasedValue {
    ptr: NonNull<u8>,
    layout: Layout,
    drop: Option<unsafe fn(*mut u8)>,
    /// Holds a value this storage owns.
    initialized: bool,
}

/// Storage for values of known layout, freed when dropped; the values are dropped too unless
/// they were never initialized or were handed over.
struct ErasedValues {
    values: Vec<ErasedValue>,
}

impl ErasedValues {
    fn new() -> Self {
        Self { values: Vec::new() }
    }

    /// Storage for one value of `info`; returns its address.
    fn push(&mut self, info: &ComponentInfo) -> *mut u8 {
        let layout = info.layout();
        let ptr = if layout.size() == 0 {
            NonNull::new(ptr::without_provenance_mut(layout.align())).unwrap_or(NonNull::dangling())
        } else {
            // SAFETY: non-zero size.
            NonNull::new(unsafe { alloc(layout) }).unwrap_or_else(|| handle_alloc_error(layout))
        };
        self.values.push(ErasedValue {
            ptr,
            layout,
            drop: info.drop_fn(),
            initialized: false,
        });
        ptr.as_ptr()
    }

    fn mark_initialized(&mut self) {
        self.values.last_mut().expect("pushed").initialized = true;
    }

    /// The values now belong to someone else.
    fn handed_over(&mut self) {
        for value in &mut self.values {
            value.initialized = false;
        }
    }

    fn ptrs(&self) -> Vec<*mut u8> {
        self.values.iter().map(|v| v.ptr.as_ptr()).collect()
    }
}

impl Drop for ErasedValues {
    fn drop(&mut self) {
        for value in &self.values {
            if value.initialized
                && let Some(drop) = value.drop
            {
                // SAFETY: an initialized value that was not handed over.
                unsafe { drop(value.ptr.as_ptr()) };
            }
            if value.layout.size() != 0 {
                // SAFETY: allocated with this layout in `push`.
                unsafe { dealloc(value.ptr.as_ptr(), value.layout) };
            }
        }
    }
}

impl World {
    /// The entity's component `id`.
    pub fn get_by_id(&self, entity: Entity, id: ComponentId) -> Option<ComponentPtr<'_>> {
        let (ptr, _) = self.component_cells(entity, id)?;
        Some(ComponentPtr {
            ptr,
            info: self.components.info(id),
        })
    }

    /// The entity's component `id` for writing.
    pub fn get_mut_by_id(&mut self, entity: Entity, id: ComponentId) -> Option<ComponentMut<'_>> {
        let this_run = self.change_tick();
        let (ptr, ticks) = self.component_cells(entity, id)?;
        Some(ComponentMut {
            ptr,
            info: self.components.info(id),
            // SAFETY: exclusive access to the world; the ticks are the row's own.
            ticks: unsafe { &mut *ticks },
            this_run,
        })
    }

    /// Adds the value at `value` as component `id`, moving it in (replacing any existing one).
    /// Returns false, leaving the value with the caller, if the entity is not alive.
    ///
    /// # Safety
    /// `value` is a valid value of the component's type, which the caller does not use or drop
    /// afterwards when this returns true.
    ///
    /// # Panics
    /// If `id` is not a component of this world.
    pub unsafe fn insert_by_id(&mut self, entity: Entity, id: ComponentId, value: *mut u8) -> bool {
        // SAFETY: forwarded from the caller.
        unsafe { self.insert_by_ids(entity, &[id], &[value]) }
    }

    /// [`insert_by_id`](Self::insert_by_id) for several components at once: one archetype move.
    ///
    /// # Safety
    /// As for `insert_by_id`, for each `values[i]` as component `ids[i]`.
    ///
    /// # Panics
    /// If the lengths differ, an id is not a component of this world, or appears twice.
    pub unsafe fn insert_by_ids(
        &mut self,
        entity: Entity,
        ids: &[ComponentId],
        values: &[*mut u8],
    ) -> bool {
        assert_eq!(ids.len(), values.len(), "one value per component id");
        for &id in ids {
            assert!(
                self.components.get(id).is_some(),
                "unknown component {id:?}"
            );
        }
        let bundle = self.bundles.register_dynamic(ids);
        // SAFETY: each value is a valid value of its component, handed over (the caller).
        unsafe {
            self.insert_erased(entity, bundle, |f| {
                for (i, &value) in values.iter().enumerate() {
                    f(i, value);
                }
            })
        }
    }

    /// Removes and drops component `id`; false if the entity is dead or lacks it.
    pub fn remove_by_id(&mut self, entity: Entity, id: ComponentId) -> bool {
        // SAFETY: no destination: the value is dropped.
        unsafe { self.take_by_id(entity, id, None) }
    }

    /// Removes component `id`, moving its value to `dst`; false (writing nothing) if the entity
    /// is dead or lacks it.
    ///
    /// # Safety
    /// `dst` is writable storage for one value of the component, which the caller then owns.
    pub unsafe fn take_by_id_into(
        &mut self,
        entity: Entity,
        id: ComponentId,
        dst: *mut u8,
    ) -> bool {
        // SAFETY: forwarded from the caller.
        unsafe { self.take_by_id(entity, id, Some(dst)) }
    }

    /// Adds a reflected value as the component of its Rust type, which must be registered.
    /// Gives the value back if the entity is not alive, the type is not a registered component,
    /// or the value can neither be reset to a default after moving out nor cloned.
    pub fn insert_value(&mut self, entity: Entity, mut value: Value) -> Result<(), Value> {
        let info = value.info();
        let Some(id) = self.components.id_of_type((info.ops.rust_id)()) else {
            return Err(value);
        };
        if !self.is_alive(entity) {
            return Err(value);
        }
        let mut storage = ErasedValues::new();
        let dst = storage.push(self.components.info(id));
        let src = value.as_mut().as_ptr();
        // SAFETY: `src` is a live value of the component's type and `dst` storage for one; after
        // the move the source is refilled with a default (so `value` drops that), or the value
        // is cloned instead.
        let moved = unsafe {
            ptr::copy_nonoverlapping(src, dst, info.size);
            if (info.ops.default)(src) {
                true
            } else {
                (info.ops.clone)(src.cast_const(), dst)
            }
        };
        if !moved {
            // The bitwise copy in `dst` is not a second owner: forget it, keep `value`.
            return Err(value);
        }
        storage.mark_initialized();
        // SAFETY: `dst` holds a valid value of component `id`.
        if unsafe { self.insert_by_id(entity, id, dst) } {
            storage.handed_over();
        }
        Ok(())
    }

    /// Adds clones of reflected values as the components of their Rust types, all in one
    /// archetype move: how levels spawn entities from loaded data. Returns false, adding
    /// nothing, if the entity is not alive, a type is not a registered component or appears
    /// twice, or a value cannot be cloned.
    pub fn insert_cloned(&mut self, entity: Entity, values: &[marlin_reflect::Ref<'_>]) -> bool {
        if !self.is_alive(entity) {
            return false;
        }
        let mut ids = Vec::with_capacity(values.len());
        for value in values {
            let Some(id) = self.components.id_of_type((value.info().ops.rust_id)()) else {
                return false;
            };
            if ids.contains(&id) {
                return false;
            }
            ids.push(id);
        }
        let mut storage = ErasedValues::new();
        for (value, &id) in values.iter().zip(&ids) {
            let dst = storage.push(self.components.info(id));
            // SAFETY: `value` is a live value of the component's Rust type (its id came from
            // that type) and `dst` is storage for one.
            if !unsafe { (value.info().ops.clone)(value.as_ptr(), dst) } {
                return false;
            }
            storage.mark_initialized();
        }
        let ptrs = storage.ptrs();
        // SAFETY: each pointer holds a valid value of the matching component, handed over on
        // success.
        if unsafe { self.insert_by_ids(entity, &ids, &ptrs) } {
            storage.handed_over();
            true
        } else {
            false
        }
    }

    /// The entity's reflected components, for serializers and inspectors.
    pub fn reflect_components(
        &self,
        entity: Entity,
    ) -> impl Iterator<Item = (ComponentId, marlin_reflect::Ref<'_>)> + '_ {
        self.entity_components(entity)
            .unwrap_or(&[])
            .iter()
            .filter_map(move |&id| Some((id, self.get_by_id(entity, id)?.reflect()?)))
    }

    /// Clones of the entity's reflected, cloneable components: a snapshot that
    /// [`insert_value`](Self::insert_value) restores.
    pub fn snapshot(&self, entity: Entity) -> Vec<Value> {
        self.reflect_components(entity)
            .filter_map(|(_, r)| Value::clone_of(r))
            .collect()
    }

    /// Spawns a copy of `entity`. Every component must be cloneable
    /// ([`register_cloneable`](Self::register_cloneable), or reflected with `Clone`); `None` if
    /// one is not or the entity is not alive. Hooks run as for a spawn.
    pub fn clone_entity(&mut self, entity: Entity) -> Option<Entity> {
        let (ids, mut values) = self.clone_components(entity, false)?;
        let copy = self.spawn_empty();
        // SAFETY: each value is a fresh clone of its component.
        unsafe { self.insert_by_ids(copy, &ids, &values.ptrs()) };
        values.handed_over();
        Some(copy)
    }

    /// Spawns a copy of `entity` in another world, registering its component types there by Rust
    /// type. `None` if a component is not cloneable or has no Rust type, or the entity is not
    /// alive.
    ///
    /// Entity ids mean nothing in another world, so the hierarchy ([`Parent`],
    /// [`Children`](crate::Children)) is left out; ids inside other components are copied as
    /// they are.
    pub fn clone_entity_to(&self, entity: Entity, dst: &mut World) -> Option<Entity> {
        let (ids, mut values) = self.clone_components(entity, true)?;
        let mut dst_ids = Vec::with_capacity(ids.len());
        for &id in &ids {
            let info = self.components.info(id);
            info.type_id()?;
            dst_ids.push(dst.components.register_descriptor(info.descriptor()));
        }
        let copy = dst.spawn_empty();
        // SAFETY: each value is a fresh clone of its component, the same type in `dst`.
        unsafe { dst.insert_by_ids(copy, &dst_ids, &values.ptrs()) };
        values.handed_over();
        Some(copy)
    }

    /// Clones of the entity's components, without the hierarchy if `skip_hierarchy`.
    fn clone_components(
        &self,
        entity: Entity,
        skip_hierarchy: bool,
    ) -> Option<(Vec<ComponentId>, ErasedValues)> {
        let mut ids = self.entity_components(entity)?.to_vec();
        if skip_hierarchy {
            let hierarchy = [
                self.components.id_of::<Parent>(),
                self.components.id_of::<Children>(),
            ];
            ids.retain(|id| !hierarchy.contains(&Some(*id)));
        }
        if !ids
            .iter()
            .all(|&id| self.components.info(id).is_cloneable())
        {
            return None;
        }
        let mut values = ErasedValues::new();
        for &id in &ids {
            let info = self.components.info(id);
            let clone = info.clone_fn()?;
            let (src, _) = self.component_cells(entity, id)?;
            let dst = values.push(info);
            // SAFETY: `src` is a live value of the component; `dst` storage for one.
            if !unsafe { clone(src, dst) } {
                return None;
            }
            values.mark_initialized();
        }
        Some((ids, values))
    }
}

// --- Dynamic queries ---

#[derive(Clone, Copy, Debug)]
struct Term {
    id: ComponentId,
    write: bool,
    optional: bool,
}

/// Builds a [`DynamicQuery`].
pub struct DynamicQueryBuilder<'w> {
    world: &'w World,
    terms: Vec<Term>,
    with: Vec<ComponentId>,
    without: Vec<ComponentId>,
}

impl DynamicQueryBuilder<'_> {
    fn term(mut self, id: ComponentId, write: bool, optional: bool) -> Self {
        assert!(
            self.world.components.get(id).is_some(),
            "unknown component {id:?}"
        );
        self.terms.push(Term {
            id,
            write,
            optional,
        });
        self
    }

    /// Fetches component `id` for reading; entities without it are skipped.
    pub fn read(self, id: ComponentId) -> Self {
        self.term(id, false, false)
    }

    /// Fetches component `id` for writing.
    pub fn write(self, id: ComponentId) -> Self {
        self.term(id, true, false)
    }

    /// Fetches component `id` for reading where present.
    pub fn read_optional(self, id: ComponentId) -> Self {
        self.term(id, false, true)
    }

    pub fn write_optional(self, id: ComponentId) -> Self {
        self.term(id, true, true)
    }

    /// Only entities with component `id`.
    pub fn with(mut self, id: ComponentId) -> Self {
        self.with.push(id);
        self
    }

    /// Only entities without component `id`.
    pub fn without(mut self, id: ComponentId) -> Self {
        self.without.push(id);
        self
    }

    /// # Panics
    /// If a component is written twice, or both written and read.
    pub fn build(self) -> DynamicQuery {
        let mut access = Access::new();
        for term in &self.terms {
            let info = self.world.components.info(term.id);
            if term.write {
                access.write_key(info.access_key(), info.name());
            } else {
                access.read_key(info.access_key(), info.name());
            }
        }
        access.assert_self_consistent();
        let mut query = DynamicQuery {
            world: self.world.id(),
            terms: self.terms,
            with: self.with,
            without: self.without,
            access,
            generation: 0,
            matched: Vec::new(),
        };
        query.update(self.world);
        query
    }
}

/// A query over components named by id, iterating raw pointers. Caches its matching
/// archetypes like a [`QueryState`](crate::QueryState).
pub struct DynamicQuery {
    world: WorldId,
    terms: Vec<Term>,
    with: Vec<ComponentId>,
    without: Vec<ComponentId>,
    access: Access,
    generation: usize,
    matched: Vec<ArchetypeId>,
}

/// Where one term's values are in the current table, or its sparse set.
#[derive(Clone, Copy)]
struct TermFetch {
    data: *mut u8,
    size: usize,
    ticks: *const TickCell,
    /// Null for table storage; both null when the component is absent.
    sparse: *const SparseSet,
}

impl TermFetch {
    const ABSENT: Self = Self {
        data: ptr::null_mut(),
        size: 0,
        ticks: ptr::null(),
        sparse: ptr::null(),
    };

    /// # Safety
    /// `row` holds `entity` in the table the fetch was made for.
    unsafe fn locate(self, row: usize, entity: Entity) -> Option<(*mut u8, *mut TickCell)> {
        if !self.sparse.is_null() {
            // SAFETY: the set outlives the iteration and is not restructured meanwhile.
            let set = unsafe { &*self.sparse };
            let r = set.row(entity)?;
            // SAFETY: `r` is in bounds.
            return Some(unsafe { (set.dense.ptr(r), set.dense.ticks_ptr().add(r).cast_mut()) });
        }
        if self.ticks.is_null() {
            return None;
        }
        // SAFETY: `row` is in bounds of the column (the caller).
        Some(unsafe {
            (
                self.data.add(row * self.size),
                self.ticks.add(row).cast_mut(),
            )
        })
    }
}

/// One entity of a [`DynamicQuery`]: its id and a pointer per term, in the order the terms were
/// added.
pub struct DynamicRow<'a> {
    entity: Entity,
    values: &'a [(*mut u8, *mut TickCell)],
    terms: &'a [Term],
    infos: &'a [&'a ComponentInfo],
    change: ChangeTicks,
}

impl DynamicRow<'_> {
    pub fn entity(&self) -> Entity {
        self.entity
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Term `i`'s value; `None` for an optional term the entity lacks.
    pub fn get(&self, i: usize) -> Option<*const u8> {
        let (ptr, _) = self.values[i];
        (!ptr.is_null()).then_some(ptr.cast_const())
    }

    /// Term `i`'s value for writing, marked changed.
    ///
    /// # Panics
    /// If term `i` was added as a read.
    pub fn get_mut(&mut self, i: usize) -> Option<*mut u8> {
        assert!(
            self.terms[i].write,
            "term {i} of the dynamic query is a read"
        );
        let (ptr, ticks) = self.values[i];
        if ptr.is_null() {
            return None;
        }
        // SAFETY: the write is declared, so this row's ticks are this query's alone.
        unsafe { (*(*ticks).get()).changed = self.change.this_run };
        Some(ptr)
    }

    /// Whether term `i` changed since the reader last looked.
    pub fn is_changed(&self, i: usize) -> bool {
        let (ptr, ticks) = self.values[i];
        // SAFETY: the row's tick cell; reads exclude writers.
        !ptr.is_null() && self.change.changed(&unsafe { (*ticks).read() })
    }

    /// Term `i` through its reflected description.
    pub fn reflect(&self, i: usize) -> Option<marlin_reflect::Ref<'_>> {
        let ptr = self.get(i)?;
        let info = self.infos[i].reflect()?;
        // SAFETY: a live value of the reflected type for the row's lifetime.
        Some(unsafe { marlin_reflect::Ref::from_raw(ptr, info) })
    }

    /// Term `i` through its reflected description for writing, marked changed.
    ///
    /// # Panics
    /// If term `i` was added as a read.
    pub fn reflect_mut(&mut self, i: usize) -> Option<marlin_reflect::Mut<'_>> {
        let info = self.infos[i].reflect()?;
        let ptr = self.get_mut(i)?;
        // SAFETY: a live value of the reflected type, written by this row alone.
        Some(unsafe { marlin_reflect::Mut::from_raw(ptr, info) })
    }
}

impl DynamicQuery {
    pub fn builder(world: &World) -> DynamicQueryBuilder<'_> {
        DynamicQueryBuilder {
            world,
            terms: Vec::new(),
            with: Vec::new(),
            without: Vec::new(),
        }
    }

    /// What the query reads and writes, for declaring it on a system.
    pub fn access(&self) -> &Access {
        &self.access
    }

    /// The access keys of the components it reads and writes.
    pub fn keys(&self, world: &World) -> Vec<(AccessKey, bool)> {
        self.terms
            .iter()
            .map(|t| (world.components.info(t.id).access_key(), t.write))
            .collect()
    }

    /// Matches archetypes created since the last update.
    pub fn update(&mut self, world: &World) {
        assert!(
            self.world == world.id(),
            "a dynamic query was used with a world other than its own"
        );
        let generation = world.archetypes.generation();
        for archetype in &world.archetypes.list[self.generation..generation] {
            let has = |id: &ComponentId| archetype.contains(*id);
            if self.terms.iter().all(|t| t.optional || has(&t.id))
                && self.with.iter().all(has)
                && !self.without.iter().any(has)
            {
                self.matched.push(archetype.id());
            }
        }
        self.generation = generation;
    }

    /// Calls `f` for each matching entity.
    pub fn for_each(&mut self, world: &mut World, f: impl FnMut(DynamicRow<'_>)) {
        world.flush();
        self.update(world);
        let change = world.world_change_ticks();
        // SAFETY: the exclusive borrow of the world rules out any other access.
        unsafe { self.for_each_unchecked(world, change, f) }
    }

    /// [`for_each`](Self::for_each) through a shared borrow.
    ///
    /// # Panics
    /// If the query writes.
    pub fn for_each_ref(&mut self, world: &World, f: impl FnMut(DynamicRow<'_>)) {
        assert!(
            self.access.is_read_only(),
            "a dynamic query that writes needs `&mut World`"
        );
        self.update(world);
        // SAFETY: shared borrows of the world only read.
        unsafe { self.for_each_unchecked(world, world.world_change_ticks(), f) }
    }

    /// # Safety
    /// Updated against `world`; for the call, nothing else accesses what the query writes, and
    /// nothing writes what it reads.
    pub(crate) unsafe fn for_each_unchecked(
        &self,
        world: &World,
        change: ChangeTicks,
        mut f: impl FnMut(DynamicRow<'_>),
    ) {
        let infos: Vec<&ComponentInfo> = self
            .terms
            .iter()
            .map(|t| world.components.info(t.id))
            .collect();
        let mut fetches: Vec<TermFetch> = Vec::with_capacity(self.terms.len());
        let mut values: Vec<(*mut u8, *mut TickCell)> = Vec::with_capacity(self.terms.len());
        for &id in &self.matched {
            let archetype = &world.archetypes.list[id.index()];
            if archetype.is_empty() {
                continue;
            }
            let table = &world.tables.list[archetype.table.index()];
            fetches.clear();
            fetches.extend(self.terms.iter().zip(&infos).map(|(t, info)| {
                if let Some(column) = table.column(t.id) {
                    TermFetch {
                        data: column.ptr(0),
                        size: column.item_size(),
                        ticks: column.ticks_ptr(),
                        sparse: ptr::null(),
                    }
                } else if let Some(set) = world.sparse_sets.get(t.id) {
                    TermFetch {
                        sparse: set,
                        size: info.layout().size(),
                        ..TermFetch::ABSENT
                    }
                } else {
                    TermFetch::ABSENT
                }
            }));
            let shared = table.archetypes > 1;
            for (i, &entity) in archetype.entities.iter().enumerate() {
                let row = if shared {
                    archetype.table_rows[i] as usize
                } else {
                    i
                };
                values.clear();
                values.extend(fetches.iter().map(|fetch| {
                    // SAFETY: `row` holds `entity` in this table.
                    unsafe { fetch.locate(row, entity) }
                        .unwrap_or((ptr::null_mut(), ptr::null_mut()))
                }));
                f(DynamicRow {
                    entity,
                    values: &values,
                    terms: &self.terms,
                    infos: &infos,
                    change,
                });
            }
        }
    }
}
