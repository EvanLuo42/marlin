//! Queries: iterate entities by the components they have.

use alloc::vec::Vec;
use core::any::TypeId;
use core::marker::PhantomData;
use core::slice;

use marlin_jobs::JobSystem;

use crate::archetype::Archetype;
use crate::change::{ChangeTicks, Mut, Ref, TickCell};
use crate::component::{Component, ComponentId};
use crate::entity::Entity;
use crate::world::World;

// --- Access ---

/// The component and resource types something reads and writes, for conflict checks.
#[derive(Clone, Default, Debug)]
pub struct Access {
    reads: Vec<(TypeId, &'static str)>,
    writes: Vec<(TypeId, &'static str)>,
}

impl Access {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read<T: 'static>(&mut self) -> &mut Self {
        self.reads
            .push((TypeId::of::<T>(), core::any::type_name::<T>()));
        self
    }

    pub fn write<T: 'static>(&mut self) -> &mut Self {
        self.writes
            .push((TypeId::of::<T>(), core::any::type_name::<T>()));
        self
    }

    fn reads(&self, id: TypeId) -> bool {
        self.reads.iter().any(|&(t, _)| t == id)
    }

    fn writes(&self, id: TypeId) -> bool {
        self.writes.iter().any(|&(t, _)| t == id)
    }

    /// Whether running both at once could race: one writes what the other reads or writes.
    pub fn conflicts_with(&self, other: &Self) -> bool {
        self.writes
            .iter()
            .any(|&(t, _)| other.reads(t) || other.writes(t))
            || other.writes.iter().any(|&(t, _)| self.reads(t))
    }

    /// The first type this access needs that `declared` does not allow, if any.
    pub(crate) fn first_outside(&self, declared: &Self) -> Option<&'static str> {
        self.writes
            .iter()
            .find(|&&(t, _)| !declared.writes(t))
            .or_else(|| {
                self.reads
                    .iter()
                    .find(|&&(t, _)| !declared.reads(t) && !declared.writes(t))
            })
            .map(|&(_, name)| name)
    }

    /// Panics if a query would alias itself: a type written twice, or written and read.
    pub(crate) fn assert_self_consistent(&self) {
        for (i, &(t, name)) in self.writes.iter().enumerate() {
            assert!(
                !self.writes[i + 1..].iter().any(|&(u, _)| u == t) && !self.reads(t),
                "query accesses `{name}` mutably more than once, or both mutably and immutably",
            );
        }
    }
}

// --- Query data ---

/// What a query yields per entity: `&T`, `&mut T` (as [`Mut<T>`]), [`Ref<T>`], `Option` of
/// those, `Entity`, or a tuple of them.
///
/// # Safety
///
/// `access` must declare every component `fetch`/`item` read or write, and `item` may only create
/// references that access allows.
pub unsafe trait QueryData {
    type Item<'w>;
    type State: Copy + Send + Sync;
    type Fetch: Copy;

    fn access(access: &mut Access);
    /// `None` when a required component was never registered: then nothing matches.
    fn state(world: &World) -> Option<Self::State>;
    fn matches(state: &Self::State, archetype: &Archetype) -> bool;
    /// # Safety
    /// `matches(state, archetype)` is true.
    unsafe fn fetch(state: &Self::State, archetype: &Archetype, change: ChangeTicks)
    -> Self::Fetch;
    /// # Safety
    /// `fetch` comes from an archetype with more than `row` rows, still alive for `'w`, and no
    /// other live reference conflicts with this query's access.
    unsafe fn item<'w>(fetch: Self::Fetch, row: usize, entity: Entity) -> Self::Item<'w>;
}

/// Query data that only reads, so it may run through `&World`.
///
/// # Safety
/// `access` declares no writes.
pub unsafe trait ReadOnlyQueryData: QueryData {}

fn column_ptr(id: ComponentId, archetype: &Archetype) -> (*mut u8, *const TickCell) {
    // SAFETY (callers): `matches` guarantees the column exists.
    let column = archetype
        .column(id)
        .expect("matched archetype has the column");
    (column.ptr(0), column.ticks_ptr())
}

// SAFETY: reads `T` only, as declared.
unsafe impl<T: Component> QueryData for &T {
    type Item<'w> = &'w T;
    type State = ComponentId;
    type Fetch = *const T;

    fn access(access: &mut Access) {
        access.read::<T>();
    }

    fn state(world: &World) -> Option<ComponentId> {
        world.component_id::<T>()
    }

    fn matches(id: &ComponentId, archetype: &Archetype) -> bool {
        archetype.contains(*id)
    }

    unsafe fn fetch(id: &ComponentId, archetype: &Archetype, _: ChangeTicks) -> *const T {
        column_ptr(*id, archetype).0.cast()
    }

    unsafe fn item<'w>(fetch: *const T, row: usize, _: Entity) -> &'w T {
        // SAFETY: the caller guarantees `row` is in bounds and no conflicting `&mut` exists.
        unsafe { &*fetch.add(row) }
    }
}

// SAFETY: as above.
unsafe impl<T: Component> ReadOnlyQueryData for &T {}

/// The data and ticks of a component column, for [`Mut`] and [`Ref`].
pub struct TrackedFetch<T> {
    data: *mut T,
    ticks: *const TickCell,
    change: ChangeTicks,
}

impl<T> Clone for TrackedFetch<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for TrackedFetch<T> {}

fn tracked<T>(id: ComponentId, archetype: &Archetype, change: ChangeTicks) -> TrackedFetch<T> {
    let (data, ticks) = column_ptr(id, archetype);
    TrackedFetch {
        data: data.cast(),
        ticks,
        change,
    }
}

impl<T> TrackedFetch<T> {
    /// # Safety
    /// As for [`QueryData::item`], with write access to `T`.
    unsafe fn item_mut<'w>(self, row: usize) -> Mut<'w, T> {
        // SAFETY: forwarded from the caller; the tick cell is this row's, as exclusive as `T`.
        unsafe {
            Mut::new(
                &mut *self.data.add(row),
                &mut *(*self.ticks.add(row)).get(),
                self.change,
            )
        }
    }

    /// # Safety
    /// As for [`QueryData::item`], with read access to `T`.
    unsafe fn item_ref<'w>(self, row: usize) -> Ref<'w, T> {
        // SAFETY: forwarded from the caller.
        unsafe {
            Ref::new(
                &*self.data.add(row),
                (*self.ticks.add(row)).read(),
                self.change,
            )
        }
    }
}

// SAFETY: writes `T` (and its ticks) only, as declared.
unsafe impl<T: Component> QueryData for &mut T {
    type Item<'w> = Mut<'w, T>;
    type State = ComponentId;
    type Fetch = TrackedFetch<T>;

    fn access(access: &mut Access) {
        access.write::<T>();
    }

    fn state(world: &World) -> Option<ComponentId> {
        world.component_id::<T>()
    }

    fn matches(id: &ComponentId, archetype: &Archetype) -> bool {
        archetype.contains(*id)
    }

    unsafe fn fetch(id: &ComponentId, archetype: &Archetype, change: ChangeTicks) -> Self::Fetch {
        tracked(*id, archetype, change)
    }

    unsafe fn item<'w>(fetch: Self::Fetch, row: usize, _: Entity) -> Mut<'w, T> {
        // SAFETY: the caller guarantees `row` is in bounds and this is the only reference to it.
        unsafe { fetch.item_mut(row) }
    }
}

// SAFETY: reads `T` and its ticks only, as declared.
unsafe impl<T: Component> QueryData for Ref<'_, T> {
    type Item<'w> = Ref<'w, T>;
    type State = ComponentId;
    type Fetch = TrackedFetch<T>;

    fn access(access: &mut Access) {
        access.read::<T>();
    }

    fn state(world: &World) -> Option<ComponentId> {
        world.component_id::<T>()
    }

    fn matches(id: &ComponentId, archetype: &Archetype) -> bool {
        archetype.contains(*id)
    }

    unsafe fn fetch(id: &ComponentId, archetype: &Archetype, change: ChangeTicks) -> Self::Fetch {
        tracked(*id, archetype, change)
    }

    unsafe fn item<'w>(fetch: Self::Fetch, row: usize, _: Entity) -> Ref<'w, T> {
        // SAFETY: the caller guarantees `row` is in bounds and no conflicting `&mut` exists.
        unsafe { fetch.item_ref(row) }
    }
}

// SAFETY: as above.
unsafe impl<T: Component> ReadOnlyQueryData for Ref<'_, T> {}

// SAFETY: reads `T` only, as declared.
unsafe impl<T: Component> QueryData for Option<&T> {
    type Item<'w> = Option<&'w T>;
    type State = Option<ComponentId>;
    type Fetch = Option<*const T>;

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
        archetype: &Archetype,
        _: ChangeTicks,
    ) -> Option<*const T> {
        id.and_then(|id| archetype.column(id))
            .map(|c| c.ptr(0).cast_const().cast())
    }

    unsafe fn item<'w>(fetch: Option<*const T>, row: usize, _: Entity) -> Option<&'w T> {
        // SAFETY: as for `&T`.
        fetch.map(|p| unsafe { &*p.add(row) })
    }
}

// SAFETY: as above.
unsafe impl<T: Component> ReadOnlyQueryData for Option<&T> {}

// SAFETY: writes `T` (and its ticks) only, as declared.
unsafe impl<T: Component> QueryData for Option<&mut T> {
    type Item<'w> = Option<Mut<'w, T>>;
    type State = Option<ComponentId>;
    type Fetch = Option<TrackedFetch<T>>;

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
        archetype: &Archetype,
        change: ChangeTicks,
    ) -> Self::Fetch {
        id.filter(|&id| archetype.contains(id))
            .map(|id| tracked(id, archetype, change))
    }

    unsafe fn item<'w>(fetch: Self::Fetch, row: usize, _: Entity) -> Option<Mut<'w, T>> {
        // SAFETY: as for `&mut T`.
        fetch.map(|f| unsafe { f.item_mut(row) })
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

    unsafe fn fetch(_: &(), _: &Archetype, _: ChangeTicks) {}

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
                archetype: &Archetype,
                change: ChangeTicks,
            ) -> Self::Fetch {
                let ($($q,)+) = state;
                // SAFETY: every member matches since the tuple does.
                unsafe { ($($Q::fetch($q, archetype, change),)+) }
            }

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
    type State: Copy + Send + Sync;
    type Fetch: Copy;
    /// Whether [`filter`](Self::filter) may reject rows (else only archetypes are filtered).
    const ROW_FILTER: bool = false;

    /// Components the filter reads (row filters read their ticks).
    fn access(_: &mut Access) {}
    fn state(world: &World) -> Self::State;
    fn matches(state: &Self::State, archetype: &Archetype) -> bool;
    /// # Safety
    /// `matches(state, archetype)` is true.
    unsafe fn fetch(state: &Self::State, archetype: &Archetype, change: ChangeTicks)
    -> Self::Fetch;
    /// # Safety
    /// `fetch` comes from an archetype with more than `row` rows, alive and not being written
    /// where the filter reads.
    unsafe fn filter(_: Self::Fetch, _: usize) -> bool {
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

    unsafe fn fetch(_: &(), _: &Archetype, _: ChangeTicks) {}
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

    unsafe fn fetch(_: &Option<ComponentId>, _: &Archetype, _: ChangeTicks) {}
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

    unsafe fn fetch(_: &Option<ComponentId>, _: &Archetype, _: ChangeTicks) {}
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
                archetype: &Archetype,
                change: ChangeTicks,
            ) -> Self::Fetch {
                let ($($f,)+) = state;
                // SAFETY: every member matches since the tuple does.
                unsafe { ($($F::fetch($f, archetype, change),)+) }
            }

            unsafe fn filter(fetch: Self::Fetch, row: usize) -> bool {
                let ($($f,)+) = fetch;
                // SAFETY: forwarded from the caller.
                true $(&& unsafe { $F::filter($f, row) })+
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

/// Iterates the entities matching `Q` and `F`, archetype by archetype, in storage order.
pub struct QueryIter<'w, Q: QueryData, F: QueryFilter = ()> {
    archetypes: slice::Iter<'w, Archetype>,
    state: Option<(Q::State, F::State)>,
    change: ChangeTicks,
    current: Option<(Q::Fetch, F::Fetch, &'w [Entity])>,
    row: usize,
}

impl<'w, Q: QueryData, F: QueryFilter> QueryIter<'w, Q, F> {
    /// # Safety
    /// For `'w`, nothing else accesses the components `Q` writes, and nothing writes the ones it
    /// or `F` reads.
    pub(crate) unsafe fn new(world: &'w World, change: ChangeTicks) -> Self {
        query_access::<Q, F>();
        Self {
            archetypes: world.archetypes().iter(),
            state: Q::state(world).map(|q| (q, F::state(world))),
            change,
            current: None,
            row: 0,
        }
    }
}

impl<'w, Q: QueryData, F: QueryFilter> Iterator for QueryIter<'w, Q, F> {
    type Item = Q::Item<'w>;

    fn next(&mut self) -> Option<Q::Item<'w>> {
        let (q, f) = self.state?;
        loop {
            if let Some((fetch, filter, entities)) = self.current {
                while self.row < entities.len() {
                    let row = self.row;
                    self.row += 1;
                    // SAFETY: `row` is in bounds; aliasing is the constructor's contract.
                    if !F::ROW_FILTER || unsafe { F::filter(filter, row) } {
                        // SAFETY: as above.
                        return Some(unsafe { Q::item(fetch, row, entities[row]) });
                    }
                }
            }
            let archetype = self.archetypes.next()?;
            if archetype.is_empty() || !Q::matches(&q, archetype) || !F::matches(&f, archetype) {
                continue;
            }
            // SAFETY: the archetype matches both.
            self.current = Some(unsafe {
                (
                    Q::fetch(&q, archetype, self.change),
                    F::fetch(&f, archetype, self.change),
                    archetype.entities(),
                )
            });
            self.row = 0;
        }
    }
}

/// A fetch handed to worker threads; the query's access rules make that sound.
#[derive(Clone, Copy)]
struct SendFetch<T>(T);

// SAFETY: items made from the fetch are `&T` (T: Sync) or `&mut T` (T: Send) of components and
// their ticks, and each row is visited by exactly one job.
unsafe impl<T> Send for SendFetch<T> {}

impl World {
    /// The ticks a query through the world compares against: changes since the last
    /// [`clear_trackers`](Self::clear_trackers).
    fn world_change_ticks(&self) -> ChangeTicks {
        ChangeTicks {
            last_run: self.last_change_tick(),
            this_run: self.change_tick(),
        }
    }

    /// Iterates entities matching `Q`, e.g. `world.query::<(&mut Position, &Velocity)>()`.
    ///
    /// # Panics
    /// If `Q` accesses a component mutably twice, or both mutably and immutably.
    pub fn query<Q: QueryData>(&mut self) -> QueryIter<'_, Q> {
        let change = self.world_change_ticks();
        // SAFETY: the exclusive borrow of the world rules out any other access.
        unsafe { QueryIter::new(self, change) }
    }

    /// [`query`](Self::query) restricted by a filter such as `With<T>`, `Changed<T>` or
    /// `(Without<A>, With<B>)`.
    pub fn query_filtered<Q: QueryData, F: QueryFilter>(&mut self) -> QueryIter<'_, Q, F> {
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
        let filter = Filter::state(self);
        let batch = batch.max(1);
        jobs.scope(|s| {
            for archetype in self.archetypes() {
                if archetype.is_empty()
                    || !Q::matches(&q, archetype)
                    || !Filter::matches(&filter, archetype)
                {
                    continue;
                }
                // SAFETY: the archetype matches both.
                let fetch = SendFetch(unsafe {
                    (
                        Q::fetch(&q, archetype, change),
                        Filter::fetch(&filter, archetype, change),
                    )
                });
                let entities = archetype.entities();
                for start in (0..entities.len()).step_by(batch) {
                    let end = entities.len().min(start + batch);
                    s.spawn(move || {
                        // The whole wrapper moves in, not its (non-`Send`) field.
                        let fetch = fetch;
                        let (fetch, rows) = fetch.0;
                        for (row, &entity) in entities.iter().enumerate().take(end).skip(start) {
                            // SAFETY: rows are split between jobs, so no two items alias; the
                            // rest is the caller's contract.
                            unsafe {
                                if !Filter::ROW_FILTER || Filter::filter(rows, row) {
                                    f(Q::item(fetch, row, entity));
                                }
                            }
                        }
                    });
                }
            }
        });
    }
}
