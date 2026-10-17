//! Queries: iterate entities by the components they have.

use alloc::vec::Vec;
use core::any::TypeId;
use core::marker::PhantomData;
use core::ptr;

use marlin_jobs::JobSystem;

use crate::archetype::{Archetype, ArchetypeId};
use crate::change::{ChangeTicks, Mut, Ref, TickCell};
use crate::component::{Component, ComponentId};
use crate::entity::Entity;
use crate::storage::{SparseSet, Table};
use crate::world::{World, WorldId};

// --- Access ---

/// What an access declaration names: a Rust type (a component or a resource), or a component
/// registered without one ([`ComponentInfo::access_key`](crate::ComponentInfo::access_key)).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AccessKey {
    Type(TypeId),
    Component(ComponentId),
}

impl AccessKey {
    pub fn of<T: 'static>() -> Self {
        Self::Type(TypeId::of::<T>())
    }
}

/// The component and resource types something reads and writes, for conflict checks.
#[derive(Clone, Default, Debug)]
pub struct Access {
    reads: Vec<(AccessKey, &'static str)>,
    writes: Vec<(AccessKey, &'static str)>,
    /// Reads everything (a `&World` system parameter).
    reads_all: bool,
}

impl Access {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read<T: 'static>(&mut self) -> &mut Self {
        self.read_key(AccessKey::of::<T>(), core::any::type_name::<T>())
    }

    pub fn write<T: 'static>(&mut self) -> &mut Self {
        self.write_key(AccessKey::of::<T>(), core::any::type_name::<T>())
    }

    /// A read of `key`, reported as `name` in errors.
    pub fn read_key(&mut self, key: AccessKey, name: &'static str) -> &mut Self {
        self.reads.push((key, name));
        self
    }

    pub fn write_key(&mut self, key: AccessKey, name: &'static str) -> &mut Self {
        self.writes.push((key, name));
        self
    }

    /// Reads every component and resource.
    pub fn read_all(&mut self) -> &mut Self {
        self.reads_all = true;
        self
    }

    pub fn reads_all(&self) -> bool {
        self.reads_all
    }

    /// Whether nothing is written.
    pub fn is_read_only(&self) -> bool {
        self.writes.is_empty()
    }

    fn reads(&self, key: AccessKey) -> bool {
        self.reads_all || self.reads.iter().any(|&(k, _)| k == key)
    }

    fn writes(&self, key: AccessKey) -> bool {
        self.writes.iter().any(|&(k, _)| k == key)
    }

    /// Adds everything `other` reads and writes.
    pub fn extend(&mut self, other: &Self) {
        self.reads.extend_from_slice(&other.reads);
        self.writes.extend_from_slice(&other.writes);
        self.reads_all |= other.reads_all;
    }

    /// A type one writes and the other reads or writes, if any: why the two could race.
    pub fn conflict_with(&self, other: &Self) -> Option<&'static str> {
        if let Some(&(_, name)) = self
            .writes
            .iter()
            .find(|&&(k, _)| other.reads(k) || other.writes(k))
        {
            return Some(name);
        }
        other
            .writes
            .iter()
            .find(|&&(k, _)| self.reads(k))
            .map(|&(_, name)| name)
    }

    /// Whether running both at once could race: one writes what the other reads or writes.
    pub fn conflicts_with(&self, other: &Self) -> bool {
        self.conflict_with(other).is_some()
    }

    /// The first type this access needs that `declared` does not allow, if any.
    pub(crate) fn first_outside(&self, declared: &Self) -> Option<&'static str> {
        if self.reads_all && !declared.reads_all {
            return Some("the whole world");
        }
        self.writes
            .iter()
            .find(|&&(k, _)| !declared.writes(k))
            .or_else(|| {
                self.reads
                    .iter()
                    .find(|&&(k, _)| !declared.reads(k) && !declared.writes(k))
            })
            .map(|&(_, name)| name)
    }

    /// Panics if a query would alias itself: a type written twice, or written and read.
    pub(crate) fn assert_self_consistent(&self) {
        for (i, &(k, name)) in self.writes.iter().enumerate() {
            assert!(
                !self.writes[i + 1..].iter().any(|&(u, _)| u == k) && !self.reads(k),
                "query accesses `{name}` mutably more than once, or both mutably and immutably",
            );
        }
    }
}

// --- Query data ---

/// What a query yields per entity: `&T`, `&mut T` (as [`Mut<T>`]), [`Ref<T>`], `Option` of
/// those, `Entity`, or a tuple of them.
///
/// A query visits the matching [`Archetype`]s; for each one it builds a `Fetch` over the
/// archetype's [`Table`] (and the world's sparse sets), then an `Item` per row.
///
/// # Safety
///
/// `access` must declare every component `fetch`/`item` read or write, and `item` may only create
/// references that access allows.
pub unsafe trait QueryData {
    type Item<'w>;
    type State: Copy + Send + Sync + PartialEq;
    type Fetch: Copy;

    fn access(access: &mut Access);
    /// `None` when a required component was never registered: then nothing matches.
    fn state(world: &World) -> Option<Self::State>;
    fn matches(state: &Self::State, archetype: &Archetype) -> bool;
    /// # Safety
    /// `table` belongs to an archetype that `matches`, in `world`.
    unsafe fn fetch(
        state: &Self::State,
        world: &World,
        table: &Table,
        change: ChangeTicks,
    ) -> Self::Fetch;
    /// # Safety
    /// `fetch` comes from a table with more than `row` rows, holding `entity` at `row`, still
    /// alive for `'w`, and no other live reference conflicts with this query's access.
    unsafe fn item<'w>(fetch: Self::Fetch, row: usize, entity: Entity) -> Self::Item<'w>;
}

/// Query data that only reads, so it may run through `&World`.
///
/// # Safety
/// `access` declares no writes.
pub unsafe trait ReadOnlyQueryData: QueryData {}

/// One component's rows in the current table, or its sparse set.
pub struct ComponentFetch<T> {
    /// Table column base; unused for sparse sets.
    data: *mut T,
    ticks: *const TickCell,
    /// Null for table storage.
    sparse: *const SparseSet,
    change: ChangeTicks,
}

impl<T> Clone for ComponentFetch<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for ComponentFetch<T> {}

impl<T> ComponentFetch<T> {
    /// The component in `table`, or in its world sparse set; `None` if neither has it.
    fn new(id: ComponentId, world: &World, table: &Table, change: ChangeTicks) -> Option<Self> {
        if let Some(column) = table.column(id) {
            return Some(Self {
                data: column.ptr(0).cast(),
                ticks: column.ticks_ptr(),
                sparse: ptr::null(),
                change,
            });
        }
        world.sparse_sets.get(id).map(|set| Self {
            data: ptr::null_mut(),
            ticks: ptr::null(),
            sparse: set,
            change,
        })
    }

    /// The row's value and ticks, if the entity has the component.
    ///
    /// # Safety
    /// `row` and `entity` as for [`QueryData::item`].
    #[inline(always)]
    pub(crate) unsafe fn locate(
        self,
        row: usize,
        entity: Entity,
    ) -> Option<(*mut T, *mut TickCell)> {
        if self.sparse.is_null() {
            // SAFETY: `row` is in bounds of the column (the caller).
            return Some(unsafe { (self.data.add(row), self.ticks.add(row).cast_mut()) });
        }
        // SAFETY: the set outlives the fetch (the caller), and nothing restructures it meanwhile.
        let set = unsafe { &*self.sparse };
        set.row(entity).map(|r| {
            // SAFETY: `r` is in bounds of the set.
            unsafe {
                (
                    set.dense.ptr(r).cast(),
                    set.dense.ticks_ptr().add(r).cast_mut(),
                )
            }
        })
    }

    /// As [`locate`](Self::locate), for an entity known to have the component.
    ///
    /// # Safety
    /// As for `locate`, and the entity has the component.
    #[inline(always)]
    unsafe fn present(self, row: usize, entity: Entity) -> (*mut T, *mut TickCell) {
        // SAFETY: forwarded from the caller.
        unsafe { self.locate(row, entity) }.expect("matched entity has the component")
    }

    /// # Safety
    /// As for [`QueryData::item`], with write access to `T`.
    #[inline(always)]
    unsafe fn item_mut<'w>(self, row: usize, entity: Entity) -> Mut<'w, T> {
        // SAFETY: forwarded from the caller; the tick cell is this row's, as exclusive as `T`.
        unsafe {
            let (value, ticks) = self.present(row, entity);
            Mut::new(&mut *value, &mut *(*ticks).get(), self.change)
        }
    }

    /// # Safety
    /// As for [`QueryData::item`], with read access to `T`.
    #[inline(always)]
    unsafe fn item_ref<'w>(self, row: usize, entity: Entity) -> Ref<'w, T> {
        // SAFETY: forwarded from the caller.
        unsafe {
            let (value, ticks) = self.present(row, entity);
            Ref::new(&*value, (*ticks).read(), self.change)
        }
    }
}

fn component_fetch<T>(
    id: ComponentId,
    world: &World,
    table: &Table,
    change: ChangeTicks,
) -> ComponentFetch<T> {
    // `matches` admitted only archetypes with the component.
    ComponentFetch::new(id, world, table, change).expect("matched archetype has the component")
}

// SAFETY: reads `T` only, as declared.
unsafe impl<T: Component> QueryData for &T {
    type Item<'w> = &'w T;
    type State = ComponentId;
    type Fetch = ComponentFetch<T>;

    fn access(access: &mut Access) {
        access.read::<T>();
    }

    fn state(world: &World) -> Option<ComponentId> {
        world.component_id::<T>()
    }

    fn matches(id: &ComponentId, archetype: &Archetype) -> bool {
        archetype.contains(*id)
    }

    unsafe fn fetch(id: &ComponentId, world: &World, table: &Table, c: ChangeTicks) -> Self::Fetch {
        component_fetch(*id, world, table, c)
    }

    #[inline(always)]
    unsafe fn item<'w>(fetch: Self::Fetch, row: usize, entity: Entity) -> &'w T {
        // SAFETY: the caller guarantees `row` is in bounds and no conflicting `&mut` exists.
        unsafe { &*fetch.present(row, entity).0 }
    }
}

// SAFETY: as above.
unsafe impl<T: Component> ReadOnlyQueryData for &T {}

// SAFETY: writes `T` (and its ticks) only, as declared.
unsafe impl<T: Component> QueryData for &mut T {
    type Item<'w> = Mut<'w, T>;
    type State = ComponentId;
    type Fetch = ComponentFetch<T>;

    fn access(access: &mut Access) {
        access.write::<T>();
    }

    fn state(world: &World) -> Option<ComponentId> {
        world.component_id::<T>()
    }

    fn matches(id: &ComponentId, archetype: &Archetype) -> bool {
        archetype.contains(*id)
    }

    unsafe fn fetch(id: &ComponentId, world: &World, table: &Table, c: ChangeTicks) -> Self::Fetch {
        component_fetch(*id, world, table, c)
    }

    #[inline(always)]
    unsafe fn item<'w>(fetch: Self::Fetch, row: usize, entity: Entity) -> Mut<'w, T> {
        // SAFETY: the caller guarantees `row` is in bounds and this is the only reference to it.
        unsafe { fetch.item_mut(row, entity) }
    }
}

// SAFETY: reads `T` and its ticks only, as declared.
unsafe impl<T: Component> QueryData for Ref<'_, T> {
    type Item<'w> = Ref<'w, T>;
    type State = ComponentId;
    type Fetch = ComponentFetch<T>;

    fn access(access: &mut Access) {
        access.read::<T>();
    }

    fn state(world: &World) -> Option<ComponentId> {
        world.component_id::<T>()
    }

    fn matches(id: &ComponentId, archetype: &Archetype) -> bool {
        archetype.contains(*id)
    }

    unsafe fn fetch(id: &ComponentId, world: &World, table: &Table, c: ChangeTicks) -> Self::Fetch {
        component_fetch(*id, world, table, c)
    }

    #[inline(always)]
    unsafe fn item<'w>(fetch: Self::Fetch, row: usize, entity: Entity) -> Ref<'w, T> {
        // SAFETY: the caller guarantees `row` is in bounds and no conflicting `&mut` exists.
        unsafe { fetch.item_ref(row, entity) }
    }
}

// SAFETY: as above.
unsafe impl<T: Component> ReadOnlyQueryData for Ref<'_, T> {}

// SAFETY: reads `T` only, as declared.
unsafe impl<T: Component> QueryData for Option<&T> {
    type Item<'w> = Option<&'w T>;
    type State = Option<ComponentId>;
    type Fetch = Option<ComponentFetch<T>>;

    fn access(access: &mut Access) {
        access.read::<T>();
    }

    fn state(world: &World) -> Option<Option<ComponentId>> {
        Some(world.component_id::<T>())
    }

    fn matches(_: &Option<ComponentId>, _: &Archetype) -> bool {
        true
    }

    unsafe fn fetch(
        id: &Option<ComponentId>,
        world: &World,
        table: &Table,
        change: ChangeTicks,
    ) -> Self::Fetch {
        id.and_then(|id| ComponentFetch::new(id, world, table, change))
    }

    #[inline(always)]
    unsafe fn item<'w>(fetch: Self::Fetch, row: usize, entity: Entity) -> Option<&'w T> {
        // SAFETY: as for `&T`.
        fetch.and_then(|f| unsafe { f.locate(row, entity) }.map(|(p, _)| unsafe { &*p }))
    }
}

// SAFETY: as above.
unsafe impl<T: Component> ReadOnlyQueryData for Option<&T> {}

// SAFETY: writes `T` (and its ticks) only, as declared.
unsafe impl<T: Component> QueryData for Option<&mut T> {
    type Item<'w> = Option<Mut<'w, T>>;
    type State = Option<ComponentId>;
    type Fetch = Option<ComponentFetch<T>>;

    fn access(access: &mut Access) {
        access.write::<T>();
    }

    fn state(world: &World) -> Option<Option<ComponentId>> {
        Some(world.component_id::<T>())
    }

    fn matches(_: &Option<ComponentId>, _: &Archetype) -> bool {
        true
    }

    unsafe fn fetch(
        id: &Option<ComponentId>,
        world: &World,
        table: &Table,
        change: ChangeTicks,
    ) -> Self::Fetch {
        id.and_then(|id| ComponentFetch::new(id, world, table, change))
    }

    #[inline(always)]
    unsafe fn item<'w>(fetch: Self::Fetch, row: usize, entity: Entity) -> Option<Mut<'w, T>> {
        let fetch = fetch?;
        // SAFETY: as for `&mut T`.
        unsafe {
            let (value, ticks) = fetch.locate(row, entity)?;
            Some(Mut::new(&mut *value, &mut *(*ticks).get(), fetch.change))
        }
    }
}

// SAFETY: touches no component.
unsafe impl QueryData for Entity {
    type Item<'w> = Entity;
    type State = ();
    type Fetch = ();

    fn access(_: &mut Access) {}

    fn state(_: &World) -> Option<()> {
        Some(())
    }

    fn matches(_: &(), _: &Archetype) -> bool {
        true
    }

    unsafe fn fetch(_: &(), _: &World, _: &Table, _: ChangeTicks) {}

    #[inline(always)]
    unsafe fn item<'w>(_: (), _: usize, entity: Entity) -> Self::Item<'w> {
        entity
    }
}

// SAFETY: as above.
unsafe impl ReadOnlyQueryData for Entity {}

macro_rules! tuple_query {
    ($(($Q:ident, $q:ident)),+) => {
        // SAFETY: the union of the members' accesses, each member upholding its own.
        unsafe impl<$($Q: QueryData),+> QueryData for ($($Q,)+) {
            type Item<'w> = ($($Q::Item<'w>,)+);
            type State = ($($Q::State,)+);
            type Fetch = ($($Q::Fetch,)+);

            fn access(access: &mut Access) {
                $($Q::access(access);)+
            }

            fn state(world: &World) -> Option<Self::State> {
                Some(($($Q::state(world)?,)+))
            }

            fn matches(state: &Self::State, archetype: &Archetype) -> bool {
                let ($($q,)+) = state;
                true $(&& $Q::matches($q, archetype))+
            }

            unsafe fn fetch(
                state: &Self::State,
                world: &World,
                table: &Table,
                change: ChangeTicks,
            ) -> Self::Fetch {
                let ($($q,)+) = state;
                // SAFETY: every member matches since the tuple does.
                unsafe { ($($Q::fetch($q, world, table, change),)+) }
            }

            #[inline(always)]
            unsafe fn item<'w>(fetch: Self::Fetch, row: usize, entity: Entity) -> Self::Item<'w> {
                let ($($q,)+) = fetch;
                // SAFETY: forwarded from the caller; members' accesses are disjoint (checked when
                // the query is built).
                unsafe { ($($Q::item($q, row, entity),)+) }
            }
        }

        // SAFETY: every member only reads.
        unsafe impl<$($Q: ReadOnlyQueryData),+> ReadOnlyQueryData for ($($Q,)+) {}
    };
}

tuple_query!((A, a));
tuple_query!((A, a), (B, b));
tuple_query!((A, a), (B, b), (C, c));
tuple_query!((A, a), (B, b), (C, c), (D, d));
tuple_query!((A, a), (B, b), (C, c), (D, d), (E, e));
tuple_query!((A, a), (B, b), (C, c), (D, d), (E, e), (F, f));
tuple_query!((A, a), (B, b), (C, c), (D, d), (E, e), (F, f), (G, g));
tuple_query!(
    (A, a),
    (B, b),
    (C, c),
    (D, d),
    (E, e),
    (F, f),
    (G, g),
    (H, h)
);

// --- Filters ---

/// Restricts which entities a query visits: by archetype ([`With`], [`Without`]) or row by row
/// ([`Changed`](crate::Changed), [`Added`](crate::Added)).
pub trait QueryFilter {
    type State: Copy + Send + Sync + PartialEq;
    type Fetch: Copy;
    /// Whether [`filter`](Self::filter) may reject rows (else only archetypes are filtered).
    const ROW_FILTER: bool = false;

    /// Components the filter reads (row filters read their ticks).
    fn access(_: &mut Access) {}
    fn state(world: &World) -> Self::State;
    fn matches(state: &Self::State, archetype: &Archetype) -> bool;
    /// # Safety
    /// `table` belongs to an archetype that `matches`, in `world`.
    unsafe fn fetch(
        state: &Self::State,
        world: &World,
        table: &Table,
        change: ChangeTicks,
    ) -> Self::Fetch;
    /// # Safety
    /// `fetch` comes from a table with more than `row` rows, holding `entity` at `row`, alive and
    /// not being written where the filter reads.
    unsafe fn filter(_: Self::Fetch, _: usize, _: Entity) -> bool {
        true
    }
}

impl QueryFilter for () {
    type State = ();
    type Fetch = ();

    fn state(_: &World) {}

    fn matches(_: &(), _: &Archetype) -> bool {
        true
    }

    unsafe fn fetch(_: &(), _: &World, _: &Table, _: ChangeTicks) {}
}

/// Entities that have a `T`.
pub struct With<T>(PhantomData<fn() -> T>);

impl<T: Component> QueryFilter for With<T> {
    type State = Option<ComponentId>;
    type Fetch = ();

    fn state(world: &World) -> Option<ComponentId> {
        world.component_id::<T>()
    }

    fn matches(id: &Option<ComponentId>, archetype: &Archetype) -> bool {
        id.is_some_and(|id| archetype.contains(id))
    }

    unsafe fn fetch(_: &Option<ComponentId>, _: &World, _: &Table, _: ChangeTicks) {}
}

/// Entities without a `T`.
pub struct Without<T>(PhantomData<fn() -> T>);

impl<T: Component> QueryFilter for Without<T> {
    type State = Option<ComponentId>;
    type Fetch = ();

    fn state(world: &World) -> Option<ComponentId> {
        world.component_id::<T>()
    }

    fn matches(id: &Option<ComponentId>, archetype: &Archetype) -> bool {
        id.is_none_or(|id| !archetype.contains(id))
    }

    unsafe fn fetch(_: &Option<ComponentId>, _: &World, _: &Table, _: ChangeTicks) {}
}

macro_rules! tuple_filter {
    ($(($F:ident, $f:ident)),+) => {
        impl<$($F: QueryFilter),+> QueryFilter for ($($F,)+) {
            type State = ($($F::State,)+);
            type Fetch = ($($F::Fetch,)+);
            const ROW_FILTER: bool = false $(|| $F::ROW_FILTER)+;

            fn access(access: &mut Access) {
                $($F::access(access);)+
            }

            fn state(world: &World) -> Self::State {
                ($($F::state(world),)+)
            }

            fn matches(state: &Self::State, archetype: &Archetype) -> bool {
                let ($($f,)+) = state;
                true $(&& $F::matches($f, archetype))+
            }

            unsafe fn fetch(
                state: &Self::State,
                world: &World,
                table: &Table,
                change: ChangeTicks,
            ) -> Self::Fetch {
                let ($($f,)+) = state;
                // SAFETY: every member matches since the tuple does.
                unsafe { ($($F::fetch($f, world, table, change),)+) }
            }

            unsafe fn filter(fetch: Self::Fetch, row: usize, entity: Entity) -> bool {
                let ($($f,)+) = fetch;
                // SAFETY: forwarded from the caller.
                true $(&& unsafe { $F::filter($f, row, entity) })+
            }
        }
    };
}

tuple_filter!((A, a));
tuple_filter!((A, a), (B, b));
tuple_filter!((A, a), (B, b), (C, c));
tuple_filter!((A, a), (B, b), (C, c), (D, d));

/// Everything a query and its filter touch. Panics if the query could alias itself: a type
/// written twice, or written and read. A filter may read what the query writes: it tests a
/// row's ticks before the row's item exists.
pub(crate) fn query_access<Q: QueryData, F: QueryFilter>() -> Access {
    let mut access = Access::new();
    Q::access(&mut access);
    access.assert_self_consistent();
    F::access(&mut access);
    access
}

// --- Iteration ---

/// Which archetypes a query iterator walks.
enum Archetypes<'w> {
    /// Every archetype, matching each one (an uncached query).
    All(core::slice::Iter<'w, Archetype>),
    /// The ones a [`QueryState`] matched.
    Matched(core::slice::Iter<'w, ArchetypeId>),
}

/// The archetype being walked.
struct Current<'w, Q: QueryData, F: QueryFilter> {
    fetch: Q::Fetch,
    filter: F::Fetch,
    entities: &'w [Entity],
    /// The table row of each entity; `None` when the table is the archetype's alone, so rows match.
    rows: Option<&'w [u32]>,
}

impl<Q: QueryData, F: QueryFilter> Clone for Current<'_, Q, F> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Q: QueryData, F: QueryFilter> Copy for Current<'_, Q, F> {}

/// Iterates the entities matching `Q` and `F`, archetype by archetype, in storage order.
pub struct QueryIter<'w, Q: QueryData, F: QueryFilter = ()> {
    world: &'w World,
    archetypes: Archetypes<'w>,
    state: Option<(Q::State, F::State)>,
    change: ChangeTicks,
    current: Option<Current<'w, Q, F>>,
    row: usize,
}

impl<'w, Q: QueryData, F: QueryFilter> QueryIter<'w, Q, F> {
    /// An uncached iterator, matching every archetype as it goes.
    ///
    /// # Safety
    /// For `'w`, nothing else accesses the components `Q` writes, and nothing writes the ones it
    /// or `F` reads.
    pub(crate) unsafe fn new(world: &'w World, change: ChangeTicks) -> Self {
        query_access::<Q, F>();
        Self {
            world,
            archetypes: Archetypes::All(world.archetypes.list.iter()),
            state: Q::state(world).map(|q| (q, F::state(world))),
            change,
            current: None,
            row: 0,
        }
    }

    /// The next matching archetype with entities.
    #[inline]
    fn next_archetype(&mut self) -> Option<&'w Archetype> {
        let (q, f) = self.state.as_ref()?;
        match &mut self.archetypes {
            Archetypes::All(iter) => {
                iter.find(|a| !a.is_empty() && Q::matches(q, a) && F::matches(f, a))
            }
            Archetypes::Matched(iter) => {
                let list = &self.world.archetypes.list;
                iter.map(|id| &list[id.index()]).find(|a| !a.is_empty())
            }
        }
    }

    /// # Safety
    /// The archetype matches; aliasing is the constructor's contract.
    #[inline]
    unsafe fn begin(&self, archetype: &'w Archetype) -> Current<'w, Q, F> {
        let (q, f) = self.state.as_ref().expect("matched with a state");
        let table = &self.world.tables.list[archetype.table.index()];
        // SAFETY: the archetype matches both.
        unsafe {
            Current {
                fetch: Q::fetch(q, self.world, table, self.change),
                filter: F::fetch(f, self.world, table, self.change),
                entities: &archetype.entities,
                rows: (table.archetypes > 1).then_some(&archetype.table_rows[..]),
            }
        }
    }

    /// Runs `g` over rows `start..` of `current`.
    ///
    /// # Safety
    /// As for the constructor.
    #[inline(always)]
    unsafe fn fold_current<B>(
        current: Current<'w, Q, F>,
        start: usize,
        mut acc: B,
        g: &mut impl FnMut(B, Q::Item<'w>) -> B,
    ) -> B {
        let entities = current.entities;
        match current.rows {
            None => {
                for (row, &entity) in entities.iter().enumerate().skip(start) {
                    // SAFETY: `row` is in bounds; aliasing is the caller's contract.
                    unsafe {
                        if !F::ROW_FILTER || F::filter(current.filter, row, entity) {
                            acc = g(acc, Q::item(current.fetch, row, entity));
                        }
                    }
                }
            }
            Some(rows) => {
                for (&row, &entity) in rows.iter().zip(entities).skip(start) {
                    let row = row as usize;
                    // SAFETY: as above.
                    unsafe {
                        if !F::ROW_FILTER || F::filter(current.filter, row, entity) {
                            acc = g(acc, Q::item(current.fetch, row, entity));
                        }
                    }
                }
            }
        }
        acc
    }
}

impl<'w, Q: QueryData, F: QueryFilter> Iterator for QueryIter<'w, Q, F> {
    type Item = Q::Item<'w>;

    #[inline]
    fn next(&mut self) -> Option<Q::Item<'w>> {
        loop {
            if let Some(current) = self.current {
                while self.row < current.entities.len() {
                    let i = self.row;
                    self.row += 1;
                    let entity = current.entities[i];
                    let row = current.rows.map_or(i, |rows| rows[i] as usize);
                    // SAFETY: `row` is in bounds; aliasing is the constructor's contract.
                    if !F::ROW_FILTER || unsafe { F::filter(current.filter, row, entity) } {
                        // SAFETY: as above.
                        return Some(unsafe { Q::item(current.fetch, row, entity) });
                    }
                }
            }
            let archetype = self.next_archetype()?;
            // SAFETY: the archetype matches.
            self.current = Some(unsafe { self.begin(archetype) });
            self.row = 0;
        }
    }

    fn fold<B, G>(mut self, init: B, mut g: G) -> B
    where
        G: FnMut(B, Self::Item) -> B,
    {
        let mut acc = init;
        if let Some(current) = self.current.take() {
            // SAFETY: the constructor's contract.
            acc = unsafe { Self::fold_current(current, self.row, acc, &mut g) };
        }
        while let Some(archetype) = self.next_archetype() {
            // SAFETY: the archetype matches; the constructor's contract.
            acc = unsafe { Self::fold_current(self.begin(archetype), 0, acc, &mut g) };
        }
        acc
    }
}

/// A fetch handed to worker threads; the query's access rules make that sound.
#[derive(Clone, Copy)]
struct SendFetch<T>(T);

// SAFETY: items made from the fetch are `&T` (T: Sync) or `&mut T` (T: Send) of components and
// their ticks, and each row is visited by exactly one job.
unsafe impl<T> Send for SendFetch<T> {}

/// Runs `f` on every entity of the archetypes, in jobs of up to `batch` rows.
///
/// # Safety
/// The archetypes match `state`; as for [`QueryIter::new`] for the duration of the call.
unsafe fn par_for_each_in<'w, Q, Filter, F>(
    world: &'w World,
    archetypes: impl Iterator<Item = &'w Archetype>,
    state: &(Q::State, Filter::State),
    jobs: &JobSystem,
    batch: usize,
    change: ChangeTicks,
    f: &F,
) where
    Q: QueryData,
    Filter: QueryFilter,
    F: for<'a> Fn(Q::Item<'a>) + Sync,
{
    let (q, filter) = state;
    let batch = batch.max(1);
    jobs.scope(|s| {
        for archetype in archetypes {
            if archetype.is_empty() {
                continue;
            }
            let table = &world.tables.list[archetype.table.index()];
            // SAFETY: the archetype matches both.
            let fetch = SendFetch(unsafe {
                (
                    Q::fetch(q, world, table, change),
                    Filter::fetch(filter, world, table, change),
                )
            });
            let entities = &archetype.entities[..];
            let rows = (table.archetypes > 1).then_some(&archetype.table_rows[..]);
            for start in (0..entities.len()).step_by(batch) {
                let end = entities.len().min(start + batch);
                s.spawn(move || {
                    // The whole wrapper moves in, not its (non-`Send`) field.
                    let fetch = fetch;
                    let (fetch, filter) = fetch.0;
                    for (i, &entity) in entities.iter().enumerate().take(end).skip(start) {
                        let row = rows.map_or(i, |rows| rows[i] as usize);
                        // SAFETY: rows are split between jobs, so no two items alias; the
                        // rest is the caller's contract.
                        unsafe {
                            if !Filter::ROW_FILTER || Filter::filter(filter, row, entity) {
                                f(Q::item(fetch, row, entity));
                            }
                        }
                    }
                });
            }
        }
    });
}

// --- Cached queries ---

/// A query that remembers which archetypes match it. Archetypes are only ever added, so on each
/// use it looks only at those created since the last one (compared by the world's archetype
/// count, its *generation*), instead of matching every archetype again.
///
/// Belongs to the world it was created from; using it with another world panics.
pub struct QueryState<Q: QueryData, F: QueryFilter = ()> {
    world: WorldId,
    state: Option<(Q::State, F::State)>,
    /// Archetypes looked at so far.
    generation: usize,
    matched: Vec<ArchetypeId>,
    /// One bit per archetype index, for `get`.
    matched_bits: Vec<u64>,
    _marker: PhantomData<fn() -> (Q, F)>,
}

impl<Q: QueryData, F: QueryFilter> QueryState<Q, F> {
    /// # Panics
    /// If `Q` accesses a component mutably twice, or both mutably and immutably.
    pub fn new(world: &World) -> Self {
        query_access::<Q, F>();
        let mut state = Self {
            world: world.id(),
            state: None,
            generation: 0,
            matched: Vec::new(),
            matched_bits: Vec::new(),
            _marker: PhantomData,
        };
        state.update(world);
        state
    }

    /// Matches archetypes created since the last update.
    pub fn update(&mut self, world: &World) {
        assert!(
            self.world == world.id(),
            "a query state was used with a world other than its own"
        );
        let generation = world.archetypes.generation();
        if generation == self.generation {
            return;
        }
        // Component ids appear as types are registered, so the state may have changed; when it
        // has, match everything again.
        let state = Q::state(world).map(|q| (q, F::state(world)));
        if state != self.state {
            self.state = state;
            self.matched.clear();
            self.matched_bits.clear();
            self.generation = 0;
        }
        if let Some((q, f)) = &self.state {
            for archetype in &world.archetypes.list[self.generation..] {
                if Q::matches(q, archetype) && F::matches(f, archetype) {
                    let index = archetype.id().index();
                    self.matched.push(archetype.id());
                    if self.matched_bits.len() <= index / 64 {
                        self.matched_bits.resize(index / 64 + 1, 0);
                    }
                    self.matched_bits[index / 64] |= 1 << (index % 64);
                }
            }
        }
        self.generation = generation;
    }

    /// The archetypes matched so far.
    pub fn matched_archetypes(&self) -> &[ArchetypeId] {
        &self.matched
    }

    #[inline]
    fn matches_archetype(&self, id: ArchetypeId) -> bool {
        let index = id.index();
        self.matched_bits
            .get(index / 64)
            .is_some_and(|bits| bits & (1 << (index % 64)) != 0)
    }

    /// Iterates through a shared borrow of the world.
    pub fn iter<'w>(&'w mut self, world: &'w World) -> QueryIter<'w, Q, F>
    where
        Q: ReadOnlyQueryData,
    {
        self.update(world);
        // SAFETY: shared borrows of the world only read.
        unsafe { self.iter_unchecked(world, world.world_change_ticks()) }
    }

    pub fn iter_mut<'w>(&'w mut self, world: &'w mut World) -> QueryIter<'w, Q, F> {
        self.update(world);
        let change = world.world_change_ticks();
        // SAFETY: the exclusive borrow of the world rules out any other access.
        unsafe { self.iter_unchecked(world, change) }
    }

    /// The entity's item, if it matches.
    pub fn get<'w>(&mut self, world: &'w World, entity: Entity) -> Option<Q::Item<'w>>
    where
        Q: ReadOnlyQueryData,
    {
        self.update(world);
        // SAFETY: shared borrows of the world only read.
        unsafe { self.get_unchecked(world, entity, world.world_change_ticks()) }
    }

    pub fn get_mut<'w>(&mut self, world: &'w mut World, entity: Entity) -> Option<Q::Item<'w>> {
        self.update(world);
        let change = world.world_change_ticks();
        // SAFETY: the exclusive borrow of the world rules out any other access.
        unsafe { self.get_unchecked(world, entity, change) }
    }

    /// [`World::par_for_each`] with the cached matches.
    pub fn par_for_each<F2>(&mut self, world: &mut World, jobs: &JobSystem, batch: usize, f: F2)
    where
        F2: for<'a> Fn(Q::Item<'a>) + Sync,
    {
        self.update(world);
        let change = world.world_change_ticks();
        // SAFETY: the exclusive borrow of the world rules out any other access.
        unsafe { self.par_for_each_unchecked(world, jobs, batch, change, &f) }
    }

    /// # Safety
    /// Updated against `world`; for `'w`, as for [`QueryIter::new`].
    pub(crate) unsafe fn iter_unchecked<'w>(
        &'w self,
        world: &'w World,
        change: ChangeTicks,
    ) -> QueryIter<'w, Q, F> {
        debug_assert_eq!(self.generation, world.archetypes.generation());
        QueryIter {
            world,
            archetypes: Archetypes::Matched(self.matched.iter()),
            state: self.state,
            change,
            current: None,
            row: 0,
        }
    }

    /// # Safety
    /// Updated against `world`; for `'w`, as for [`QueryIter::new`].
    pub(crate) unsafe fn get_unchecked<'w>(
        &self,
        world: &'w World,
        entity: Entity,
        change: ChangeTicks,
    ) -> Option<Q::Item<'w>> {
        let location = world.entities.location(entity)?;
        if !self.matches_archetype(location.archetype) {
            return None;
        }
        let (q, f) = self.state.as_ref()?;
        let table = &world.tables.list[location.table.index()];
        let row = location.table_row as usize;
        // SAFETY: the archetype matches; aliasing is the caller's contract.
        unsafe {
            let filter = F::fetch(f, world, table, change);
            if F::ROW_FILTER && !F::filter(filter, row, entity) {
                return None;
            }
            Some(Q::item(Q::fetch(q, world, table, change), row, entity))
        }
    }

    /// Whether the entity matches, testing only archetypes and the filter.
    ///
    /// # Safety
    /// Updated against `world`; nothing writes what the filter reads for the call.
    pub(crate) unsafe fn contains_unchecked(
        &self,
        world: &World,
        entity: Entity,
        change: ChangeTicks,
    ) -> bool {
        let Some(location) = world.entities.location(entity) else {
            return false;
        };
        if !self.matches_archetype(location.archetype) {
            return false;
        }
        if !F::ROW_FILTER {
            return true;
        }
        let Some((_, f)) = &self.state else {
            return false;
        };
        let table = &world.tables.list[location.table.index()];
        // SAFETY: the archetype matches; forwarded from the caller.
        unsafe {
            F::filter(
                F::fetch(f, world, table, change),
                location.table_row as usize,
                entity,
            )
        }
    }

    /// Matching entities, testing only archetypes and the filter.
    ///
    /// # Safety
    /// As for [`contains_unchecked`](Self::contains_unchecked).
    pub(crate) unsafe fn count_unchecked(&self, world: &World, change: ChangeTicks) -> usize {
        let list = &world.archetypes.list;
        let archetypes = self.matched.iter().map(|id| &list[id.index()]);
        if !F::ROW_FILTER {
            return archetypes.map(Archetype::len).sum();
        }
        let Some((_, f)) = &self.state else { return 0 };
        let mut count = 0;
        for archetype in archetypes.filter(|a| !a.is_empty()) {
            let table = &world.tables.list[archetype.table.index()];
            // SAFETY: the archetype matches; forwarded from the caller.
            let fetch = unsafe { F::fetch(f, world, table, change) };
            let shared = table.archetypes > 1;
            for (i, &entity) in archetype.entities.iter().enumerate() {
                let row = if shared {
                    archetype.table_rows[i] as usize
                } else {
                    i
                };
                // SAFETY: as above.
                count += usize::from(unsafe { F::filter(fetch, row, entity) });
            }
        }
        count
    }

    /// # Safety
    /// Updated against `world`; as for [`QueryIter::new`] for the duration of the call.
    pub(crate) unsafe fn par_for_each_unchecked<F2>(
        &self,
        world: &World,
        jobs: &JobSystem,
        batch: usize,
        change: ChangeTicks,
        f: &F2,
    ) where
        F2: for<'a> Fn(Q::Item<'a>) + Sync,
    {
        let Some(state) = &self.state else { return };
        let list = &world.archetypes.list;
        let archetypes = self.matched.iter().map(|id| &list[id.index()]);
        // SAFETY: matched archetypes; forwarded from the caller.
        unsafe { par_for_each_in::<Q, F, F2>(world, archetypes, state, jobs, batch, change, f) }
    }
}

impl World {
    /// The ticks a query through the world compares against: changes since the last
    /// [`clear_trackers`](Self::clear_trackers).
    pub(crate) fn world_change_ticks(&self) -> ChangeTicks {
        ChangeTicks {
            last_run: self.last_change_tick(),
            this_run: self.change_tick(),
        }
    }

    /// Iterates entities matching `Q`, e.g. `world.query::<(&mut Position, &Velocity)>()`. This
    /// matches every archetype on each call; keep a [`QueryState`] ([`query_state`]) to match only
    /// new ones.
    ///
    /// [`query_state`]: Self::query_state
    ///
    /// # Panics
    /// If `Q` accesses a component mutably twice, or both mutably and immutably.
    pub fn query<Q: QueryData>(&mut self) -> QueryIter<'_, Q> {
        self.flush();
        let change = self.world_change_ticks();
        // SAFETY: the exclusive borrow of the world rules out any other access.
        unsafe { QueryIter::new(self, change) }
    }

    /// [`query`](Self::query) restricted by a filter such as `With<T>`, `Changed<T>` or
    /// `(Without<A>, With<B>)`.
    pub fn query_filtered<Q: QueryData, F: QueryFilter>(&mut self) -> QueryIter<'_, Q, F> {
        self.flush();
        let change = self.world_change_ticks();
        // SAFETY: as above.
        unsafe { QueryIter::new(self, change) }
    }

    /// A read-only query through a shared borrow.
    pub fn query_ref<Q: ReadOnlyQueryData>(&self) -> QueryIter<'_, Q> {
        // SAFETY: shared borrows of the world only read.
        unsafe { QueryIter::new(self, self.world_change_ticks()) }
    }

    pub fn query_ref_filtered<Q: ReadOnlyQueryData, F: QueryFilter>(&self) -> QueryIter<'_, Q, F> {
        // SAFETY: as above.
        unsafe { QueryIter::new(self, self.world_change_ticks()) }
    }

    /// A cached query: see [`QueryState`].
    pub fn query_state<Q: QueryData, F: QueryFilter>(&self) -> QueryState<Q, F> {
        QueryState::new(self)
    }

    /// Runs `f` on every entity matching `Q`, in parallel jobs of up to `batch` entities, and
    /// waits for all of them.
    pub fn par_for_each<Q, F>(&mut self, jobs: &JobSystem, batch: usize, f: F)
    where
        Q: QueryData,
        F: for<'a> Fn(Q::Item<'a>) + Sync,
    {
        self.par_for_each_filtered::<Q, (), F>(jobs, batch, f);
    }

    /// [`par_for_each`](Self::par_for_each) restricted by a filter.
    pub fn par_for_each_filtered<Q, Filter, F>(&mut self, jobs: &JobSystem, batch: usize, f: F)
    where
        Q: QueryData,
        Filter: QueryFilter,
        F: for<'a> Fn(Q::Item<'a>) + Sync,
    {
        self.flush();
        let change = self.world_change_ticks();
        // SAFETY: the exclusive borrow of the world rules out any other access.
        unsafe { self.par_for_each_unchecked::<Q, Filter, F>(jobs, batch, change, &f) }
    }

    /// # Safety
    /// As for [`QueryIter::new`], for the duration of the call.
    pub(crate) unsafe fn par_for_each_unchecked<Q, Filter, F>(
        &self,
        jobs: &JobSystem,
        batch: usize,
        change: ChangeTicks,
        f: &F,
    ) where
        Q: QueryData,
        Filter: QueryFilter,
        F: for<'a> Fn(Q::Item<'a>) + Sync,
    {
        query_access::<Q, Filter>();
        let Some(q) = Q::state(self) else { return };
        let state = (q, Filter::state(self));
        let archetypes = self
            .archetypes
            .list
            .iter()
            .filter(|a| Q::matches(&state.0, a) && Filter::matches(&state.1, a));
        // SAFETY: only matching archetypes; forwarded from the caller.
        unsafe { par_for_each_in::<Q, Filter, F>(self, archetypes, &state, jobs, batch, change, f) }
    }
}
