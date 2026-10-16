//! Queries: iterate entities by the components they have.

use alloc::vec::Vec;
use core::any::TypeId;
use core::marker::PhantomData;
use core::slice;

use marlin_jobs::JobSystem;

use crate::archetype::Archetype;
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

/// What a query yields per entity: `&T`, `&mut T`, `Option<&T>`, `Option<&mut T>`, `Entity`,
/// or a tuple of those.
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
    unsafe fn fetch(state: &Self::State, archetype: &Archetype) -> Self::Fetch;
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

    unsafe fn fetch(id: &ComponentId, archetype: &Archetype) -> *const T {
        // SAFETY: `matches` guarantees the column exists.
        unsafe { archetype.column(*id).unwrap_unchecked() }
            .ptr(0)
            .cast()
    }

    unsafe fn item<'w>(fetch: *const T, row: usize, _: Entity) -> &'w T {
        // SAFETY: the caller guarantees `row` is in bounds and no conflicting `&mut` exists.
        unsafe { &*fetch.add(row) }
    }
}

// SAFETY: as above.
unsafe impl<T: Component> ReadOnlyQueryData for &T {}

// SAFETY: writes `T` only, as declared.
unsafe impl<T: Component> QueryData for &mut T {
    type Item<'w> = &'w mut T;
    type State = ComponentId;
    type Fetch = *mut T;

    fn access(access: &mut Access) {
        access.write::<T>();
    }

    fn state(world: &World) -> Option<ComponentId> {
        world.component_id::<T>()
    }

    fn matches(id: &ComponentId, archetype: &Archetype) -> bool {
        archetype.contains(*id)
    }

    unsafe fn fetch(id: &ComponentId, archetype: &Archetype) -> *mut T {
        // SAFETY: `matches` guarantees the column exists.
        unsafe { archetype.column(*id).unwrap_unchecked() }
            .ptr(0)
            .cast()
    }

    unsafe fn item<'w>(fetch: *mut T, row: usize, _: Entity) -> &'w mut T {
        // SAFETY: the caller guarantees `row` is in bounds and this is the only reference to it.
        unsafe { &mut *fetch.add(row) }
    }
}

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

    unsafe fn fetch(id: &Option<ComponentId>, archetype: &Archetype) -> Option<*const T> {
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

// SAFETY: writes `T` only, as declared.
unsafe impl<T: Component> QueryData for Option<&mut T> {
    type Item<'w> = Option<&'w mut T>;
    type State = Option<ComponentId>;
    type Fetch = Option<*mut T>;

    fn access(access: &mut Access) {
        access.write::<T>();
    }

    fn state(world: &World) -> Option<Option<ComponentId>> {
        Some(world.component_id::<T>())
    }

    fn matches(_: &Option<ComponentId>, _: &Archetype) -> bool {
        true
    }

    unsafe fn fetch(id: &Option<ComponentId>, archetype: &Archetype) -> Option<*mut T> {
        id.and_then(|id| archetype.column(id))
            .map(|c| c.ptr(0).cast())
    }

    unsafe fn item<'w>(fetch: Option<*mut T>, row: usize, _: Entity) -> Option<&'w mut T> {
        // SAFETY: as for `&mut T`.
        fetch.map(|p| unsafe { &mut *p.add(row) })
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

    unsafe fn fetch(_: &(), _: &Archetype) {}

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

            unsafe fn fetch(state: &Self::State, archetype: &Archetype) -> Self::Fetch {
                let ($($q,)+) = state;
                // SAFETY: every member matches since the tuple does.
                unsafe { ($($Q::fetch($q, archetype),)+) }
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

/// Restricts which archetypes a query visits, without accessing their data.
pub trait QueryFilter {
    type State: Copy + Send + Sync;

    fn state(world: &World) -> Self::State;
    fn matches(state: &Self::State, archetype: &Archetype) -> bool;
}

impl QueryFilter for () {
    type State = ();

    fn state(_: &World) {}

    fn matches(_: &(), _: &Archetype) -> bool {
        true
    }
}

/// Entities that have a `T`.
pub struct With<T>(PhantomData<fn() -> T>);

impl<T: Component> QueryFilter for With<T> {
    type State = Option<ComponentId>;

    fn state(world: &World) -> Option<ComponentId> {
        world.component_id::<T>()
    }

    fn matches(id: &Option<ComponentId>, archetype: &Archetype) -> bool {
        id.is_some_and(|id| archetype.contains(id))
    }
}

/// Entities without a `T`.
pub struct Without<T>(PhantomData<fn() -> T>);

impl<T: Component> QueryFilter for Without<T> {
    type State = Option<ComponentId>;

    fn state(world: &World) -> Option<ComponentId> {
        world.component_id::<T>()
    }

    fn matches(id: &Option<ComponentId>, archetype: &Archetype) -> bool {
        id.is_none_or(|id| !archetype.contains(id))
    }
}

macro_rules! tuple_filter {
    ($(($F:ident, $f:ident)),+) => {
        impl<$($F: QueryFilter),+> QueryFilter for ($($F,)+) {
            type State = ($($F::State,)+);

            fn state(world: &World) -> Self::State {
                ($($F::state(world),)+)
            }

            fn matches(state: &Self::State, archetype: &Archetype) -> bool {
                let ($($f,)+) = state;
                true $(&& $F::matches($f, archetype))+
            }
        }
    };
}

tuple_filter!((A, a));
tuple_filter!((A, a), (B, b));
tuple_filter!((A, a), (B, b), (C, c));
tuple_filter!((A, a), (B, b), (C, c), (D, d));

// --- Iteration ---

/// Iterates the entities matching `Q` and `F`, archetype by archetype, in storage order.
pub struct QueryIter<'w, Q: QueryData, F: QueryFilter = ()> {
    archetypes: slice::Iter<'w, Archetype>,
    state: Option<(Q::State, F::State)>,
    current: Option<(Q::Fetch, &'w [Entity])>,
    row: usize,
}

impl<'w, Q: QueryData, F: QueryFilter> QueryIter<'w, Q, F> {
    /// # Safety
    /// For `'w`, nothing else accesses the components `Q` writes, and nothing writes the ones it
    /// reads.
    pub(crate) unsafe fn new(world: &'w World) -> Self {
        let mut access = Access::new();
        Q::access(&mut access);
        access.assert_self_consistent();
        Self {
            archetypes: world.archetypes().iter(),
            state: Q::state(world).map(|q| (q, F::state(world))),
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
            if let Some((fetch, entities)) = self.current
                && self.row < entities.len()
            {
                let row = self.row;
                self.row += 1;
                // SAFETY: `row` is in bounds; aliasing is the constructor's contract.
                return Some(unsafe { Q::item(fetch, row, entities[row]) });
            }
            let archetype = self.archetypes.next()?;
            if archetype.is_empty() || !Q::matches(&q, archetype) || !F::matches(&f, archetype) {
                continue;
            }
            // SAFETY: the archetype matches.
            self.current = Some((unsafe { Q::fetch(&q, archetype) }, archetype.entities()));
            self.row = 0;
        }
    }
}

/// A fetch handed to worker threads; the query's access rules make that sound.
#[derive(Clone, Copy)]
struct SendFetch<T>(T);

// SAFETY: items made from the fetch are `&T` (T: Sync) or `&mut T` (T: Send) of components, and
// each row is visited by exactly one job.
unsafe impl<T> Send for SendFetch<T> {}

impl World {
    /// Iterates entities matching `Q`, e.g. `world.query::<(&mut Position, &Velocity)>()`.
    ///
    /// # Panics
    /// If `Q` accesses a component mutably twice, or both mutably and immutably.
    pub fn query<Q: QueryData>(&mut self) -> QueryIter<'_, Q> {
        // SAFETY: the exclusive borrow of the world rules out any other access.
        unsafe { QueryIter::new(self) }
    }

    /// [`query`](Self::query) restricted by a filter such as `With<T>` or `(Without<A>, With<B>)`.
    pub fn query_filtered<Q: QueryData, F: QueryFilter>(&mut self) -> QueryIter<'_, Q, F> {
        // SAFETY: as above.
        unsafe { QueryIter::new(self) }
    }

    /// A read-only query through a shared borrow.
    pub fn query_ref<Q: ReadOnlyQueryData>(&self) -> QueryIter<'_, Q> {
        // SAFETY: shared borrows of the world only read.
        unsafe { QueryIter::new(self) }
    }

    pub fn query_ref_filtered<Q: ReadOnlyQueryData, F: QueryFilter>(&self) -> QueryIter<'_, Q, F> {
        // SAFETY: as above.
        unsafe { QueryIter::new(self) }
    }

    /// Runs `f` on every entity matching `Q`, in parallel jobs of up to `batch` entities, and
    /// waits for all of them.
    pub fn par_for_each<Q, F>(&mut self, jobs: &JobSystem, batch: usize, f: F)
    where
        Q: QueryData,
        F: for<'a> Fn(Q::Item<'a>) + Sync,
    {
        // SAFETY: the exclusive borrow of the world rules out any other access.
        unsafe { self.par_for_each_unchecked::<Q, (), F>(jobs, batch, &f) }
    }

    /// # Safety
    /// As for [`QueryIter::new`], for the duration of the call.
    pub(crate) unsafe fn par_for_each_unchecked<Q, Filter, F>(
        &self,
        jobs: &JobSystem,
        batch: usize,
        f: &F,
    ) where
        Q: QueryData,
        Filter: QueryFilter,
        F: for<'a> Fn(Q::Item<'a>) + Sync,
    {
        let mut access = Access::new();
        Q::access(&mut access);
        access.assert_self_consistent();
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
                // SAFETY: the archetype matches.
                let fetch = SendFetch(unsafe { Q::fetch(&q, archetype) });
                let entities = archetype.entities();
                for start in (0..entities.len()).step_by(batch) {
                    let end = entities.len().min(start + batch);
                    s.spawn(move || {
                        let fetch = fetch;
                        for (row, &entity) in entities.iter().enumerate().take(end).skip(start) {
                            // SAFETY: rows are split between jobs, so no two items alias; the
                            // rest is the caller's contract.
                            f(unsafe { Q::item(fetch.0, row, entity) });
                        }
                    });
                }
            }
        });
    }
}
