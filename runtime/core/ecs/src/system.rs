//! Function systems: plain functions whose parameters say what they access.
//!
//! ```
//! use marlin_ecs::{Query, Res, Schedule, World};
//! # use marlin_jobs::{Config, JobSystem};
//!
//! struct Pos(f32);
//! struct Vel(f32);
//! struct Time {
//!     dt: f32,
//! }
//!
//! fn movement(mut q: Query<(&mut Pos, &Vel)>, time: Res<Time>) {
//!     for (mut pos, vel) in &mut q {
//!         pos.0 += vel.0 * time.dt;
//!     }
//! }
//!
//! let mut world = World::new();
//! world.insert_resource(Time { dt: 0.5 });
//! let e = world.spawn((Pos(0.0), Vel(2.0)));
//! let mut schedule = Schedule::new();
//! schedule.add_system(movement);
//! # let jobs = JobSystem::new(Config { workers: 1, ..Config::default() }).unwrap();
//! schedule.run(&mut world, &jobs);
//! assert_eq!(world.get::<Pos>(e).unwrap().0, 1.0);
//! ```
//!
//! Each parameter is a [`SystemParam`]: [`Query`], [`Res`], [`ResMut`], `Option<Res<T>>`,
//! `Option<ResMut<T>>`, [`Commands`], [`Local`], [`EventReader`], [`EventWriter`],
//! [`RemovedComponents`], `&World` (reads everything), `&JobSystem`, or a tuple of them. The
//! system's access is the union of its parameters' accesses, so nothing needs declaring by hand.
//! Parameters that conflict with each other (`Query<&mut A>` beside `Query<&A>`, `Res<R>` beside
//! `ResMut<R>`) are rejected when the system is built: there is no disjointness analysis, so
//! `Query<&mut A, With<B>>` and `Query<&mut A, Without<B>>` conflict too.
//!
//! The type system rejects the rest. A query that writes cannot iterate through `&self`:
//!
//! ```compile_fail
//! # use marlin_ecs::Query;
//! # struct Pos(f32);
//! fn sneaky(q: Query<&mut Pos>) {
//!     for _ in q.iter() {}
//! }
//! ```
//!
//! Run conditions only take read-only parameters, so they cannot record commands or write
//! resources:
//!
//! ```compile_fail
//! # use marlin_ecs::{Commands, System};
//! System::new("s", |_| {}).run_if(|_: Commands| true);
//! ```
//!
//! Parameters must be system parameters; `&mut World` makes an exclusive system on its own, not
//! a parameter beside others:
//!
//! ```compile_fail
//! # use marlin_ecs::{Res, Schedule, World};
//! Schedule::new().add_system(|_: &mut World, _: Res<u32>| {});
//! ```

use alloc::boxed::Box;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};

use marlin_jobs::JobSystem;

use crate::change::{ChangeTicks, Tick};
use crate::command::{CommandQueue, Commands};
use crate::component::Component;
use crate::entity::Entity;
use crate::event::{Event, EventReader, EventWriter, Events, missing_events};
use crate::query::{
    Access, QueryData, QueryFilter, QueryIter, QueryState, ReadOnlyQueryData, query_access,
};
use crate::world::World;

/// What one run of a system gives its parameters.
pub struct SystemRun<'w> {
    pub(crate) world: &'w World,
    pub(crate) jobs: &'w JobSystem,
    pub(crate) change: ChangeTicks,
    pub(crate) name: &'static str,
}

impl<'w> SystemRun<'w> {
    pub fn world(&self) -> &'w World {
        self.world
    }

    pub fn jobs(&self) -> &'w JobSystem {
        self.jobs
    }

    /// Changes since the system last ran.
    pub fn change_ticks(&self) -> ChangeTicks {
        self.change
    }

    pub fn name(&self) -> &'static str {
        self.name
    }
}

/// A system's access as its parameters are added, rejecting parameters that conflict.
pub struct SystemAccess {
    name: &'static str,
    access: Access,
}

impl SystemAccess {
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            access: Access::new(),
        }
    }

    /// Adds one parameter's access.
    ///
    /// # Panics
    /// If it conflicts with a parameter added before.
    #[track_caller]
    pub fn add(&mut self, param: &Access) {
        if let Some(name) = param.conflict_with(&self.access) {
            panic!(
                "system `{}` has conflicting parameters: two of them access `{name}`, and at \
                 least one writes it",
                self.name
            );
        }
        self.access.extend(param);
    }

    pub(crate) fn into_access(self) -> Access {
        self.access
    }
}

/// A function-system parameter.
///
/// # Safety
///
/// `access` must declare everything `get`'s item reads or writes in the world.
pub unsafe trait SystemParam {
    /// Per-system data, made once against the world.
    type State: Send + 'static;
    type Item<'w, 's>;

    /// Adds what the parameter reads and writes; called when the system is built, without a
    /// world.
    fn access(access: &mut SystemAccess);

    fn init_state(world: &mut World) -> Self::State;

    /// # Safety
    /// `state` comes from `init_state` on `run.world`, and for `'w` nothing running alongside
    /// conflicts with `access`.
    unsafe fn get<'w, 's>(state: &'s mut Self::State, run: &SystemRun<'w>) -> Self::Item<'w, 's>;

    /// Runs after the system's wave, with the whole world: applies [`Commands`].
    fn apply(_state: &mut Self::State, _world: &mut World) {}
}

/// A parameter that never writes the world, usable in run conditions.
///
/// # Safety
/// `access` declares no writes and `apply` does nothing.
pub unsafe trait ReadOnlySystemParam: SystemParam {}

pub type SystemParamItem<'w, 's, P> = <P as SystemParam>::Item<'w, 's>;

fn access_of(f: impl FnOnce(&mut Access)) -> Access {
    let mut access = Access::new();
    f(&mut access);
    access
}

// --- Query ---

/// A system parameter iterating the entities matching `Q` and `F`, with its matching archetypes
/// cached ([`QueryState`]).
pub struct Query<'w, 's, Q: QueryData, F: QueryFilter = ()> {
    world: &'w World,
    state: &'s QueryState<Q, F>,
    change: ChangeTicks,
    jobs: &'w JobSystem,
}

// SAFETY: the access is the query's and the filter's, as `QueryState` uses them.
unsafe impl<Q: QueryData + 'static, F: QueryFilter + 'static> SystemParam for Query<'_, '_, Q, F> {
    type State = QueryState<Q, F>;
    type Item<'w, 's> = Query<'w, 's, Q, F>;

    fn access(access: &mut SystemAccess) {
        access.add(&query_access::<Q, F>());
    }

    fn init_state(world: &mut World) -> Self::State {
        QueryState::new(world)
    }

    unsafe fn get<'w, 's>(state: &'s mut Self::State, run: &SystemRun<'w>) -> Self::Item<'w, 's> {
        state.update(run.world);
        Query {
            world: run.world,
            state,
            change: run.change,
            jobs: run.jobs,
        }
    }
}

// SAFETY: read-only query data and filters only read.
unsafe impl<Q: ReadOnlyQueryData + 'static, F: QueryFilter + 'static> ReadOnlySystemParam
    for Query<'_, '_, Q, F>
{
}

impl<'w, 's, Q: QueryData, F: QueryFilter> Query<'w, 's, Q, F> {
    pub fn iter(&self) -> QueryIter<'_, Q, F>
    where
        Q: ReadOnlyQueryData,
    {
        // SAFETY: declared access (the system's); shared borrows only read.
        unsafe { self.state.iter_unchecked(self.world, self.change) }
    }

    pub fn iter_mut(&mut self) -> QueryIter<'_, Q, F> {
        // SAFETY: declared access; `&mut self` keeps the items unique.
        unsafe { self.state.iter_unchecked(self.world, self.change) }
    }

    pub fn for_each(&self, f: impl FnMut(Q::Item<'_>))
    where
        Q: ReadOnlyQueryData,
    {
        self.iter().for_each(f);
    }

    pub fn for_each_mut(&mut self, f: impl FnMut(Q::Item<'_>)) {
        self.iter_mut().for_each(f);
    }

    /// The entity's item, if it matches.
    pub fn get(&self, entity: Entity) -> Option<Q::Item<'_>>
    where
        Q: ReadOnlyQueryData,
    {
        // SAFETY: as for `iter`.
        unsafe { self.state.get_unchecked(self.world, entity, self.change) }
    }

    pub fn get_mut(&mut self, entity: Entity) -> Option<Q::Item<'_>> {
        // SAFETY: as for `iter_mut`.
        unsafe { self.state.get_unchecked(self.world, entity, self.change) }
    }

    /// Whether the entity matches.
    pub fn contains(&self, entity: Entity) -> bool {
        // SAFETY: only the filter's reads; as for `iter`.
        unsafe {
            self.state
                .contains_unchecked(self.world, entity, self.change)
        }
    }

    /// The only matching entity's item: `None` if there is none or more than one.
    pub fn single(&self) -> Option<Q::Item<'_>>
    where
        Q: ReadOnlyQueryData,
    {
        let mut iter = self.iter();
        let item = iter.next()?;
        iter.next().is_none().then_some(item)
    }

    pub fn single_mut(&mut self) -> Option<Q::Item<'_>> {
        if self.count() != 1 {
            return None;
        }
        self.iter_mut().next()
    }

    /// Matching entities.
    pub fn count(&self) -> usize {
        // SAFETY: only the filter's reads; as for `iter`.
        unsafe { self.state.count_unchecked(self.world, self.change) }
    }

    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }

    /// [`World::par_for_each`] with this query.
    pub fn par_for_each(&mut self, batch: usize, f: impl for<'a> Fn(Q::Item<'a>) + Sync) {
        // SAFETY: as for `iter_mut`; jobs split the rows.
        unsafe {
            self.state
                .par_for_each_unchecked(self.world, self.jobs, batch, self.change, &f)
        }
    }

    /// The ticks this run compares against.
    pub fn change_ticks(&self) -> ChangeTicks {
        self.change
    }
}

impl<'a, Q: ReadOnlyQueryData, F: QueryFilter> IntoIterator for &'a Query<'_, '_, Q, F> {
    type Item = Q::Item<'a>;
    type IntoIter = QueryIter<'a, Q, F>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, Q: QueryData, F: QueryFilter> IntoIterator for &'a mut Query<'_, '_, Q, F> {
    type Item = Q::Item<'a>;
    type IntoIter = QueryIter<'a, Q, F>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

// --- Resources ---

/// A system parameter reading resource `T`. The system panics if the resource does not exist;
/// use `Option<Res<T>>` if it may not.
pub struct Res<'w, T> {
    value: &'w T,
}

impl<T> Clone for Res<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Res<'_, T> {}

impl<'w, T> Res<'w, T> {
    pub fn into_inner(self) -> &'w T {
        self.value
    }
}

impl<T> Deref for Res<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.value
    }
}

/// A system parameter writing resource `T`.
pub struct ResMut<'w, T> {
    value: &'w mut T,
}

impl<'w, T> ResMut<'w, T> {
    pub fn into_inner(self) -> &'w mut T {
        self.value
    }
}

impl<T> Deref for ResMut<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.value
    }
}

impl<T> DerefMut for ResMut<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.value
    }
}

#[track_caller]
fn missing_resource<T>(system: &str) -> ! {
    panic!(
        "system `{system}` needs resource `{}`, which does not exist",
        core::any::type_name::<T>()
    )
}

// SAFETY: reads `T`, as declared.
unsafe impl<T: Send + Sync + 'static> SystemParam for Res<'_, T> {
    type State = ();
    type Item<'w, 's> = Res<'w, T>;

    fn access(access: &mut SystemAccess) {
        access.add(&access_of(|a| {
            a.read::<T>();
        }));
    }

    fn init_state(_: &mut World) {}

    unsafe fn get<'w>(_: &mut (), run: &SystemRun<'w>) -> Res<'w, T> {
        let ptr = run
            .world
            .resource_ptr::<T>()
            .unwrap_or_else(|| missing_resource::<T>(run.name));
        // SAFETY: declared read; no writer of `T` runs alongside.
        let value = unsafe { &*ptr };
        Res { value }
    }
}

// SAFETY: as above.
unsafe impl<T: Send + Sync + 'static> ReadOnlySystemParam for Res<'_, T> {}

// SAFETY: reads `T`, as declared.
unsafe impl<T: Send + Sync + 'static> SystemParam for Option<Res<'_, T>> {
    type State = ();
    type Item<'w, 's> = Option<Res<'w, T>>;

    fn access(access: &mut SystemAccess) {
        Res::<T>::access(access);
    }

    fn init_state(_: &mut World) {}

    unsafe fn get<'w>(_: &mut (), run: &SystemRun<'w>) -> Option<Res<'w, T>> {
        run.world.resource_ptr::<T>().map(|p| {
            // SAFETY: as for `Res`.
            let value = unsafe { &*p };
            Res { value }
        })
    }
}

// SAFETY: as above.
unsafe impl<T: Send + Sync + 'static> ReadOnlySystemParam for Option<Res<'_, T>> {}

// SAFETY: writes `T`, as declared.
unsafe impl<T: Send + Sync + 'static> SystemParam for ResMut<'_, T> {
    type State = ();
    type Item<'w, 's> = ResMut<'w, T>;

    fn access(access: &mut SystemAccess) {
        access.add(&access_of(|a| {
            a.write::<T>();
        }));
    }

    fn init_state(_: &mut World) {}

    unsafe fn get<'w>(_: &mut (), run: &SystemRun<'w>) -> ResMut<'w, T> {
        let ptr = run
            .world
            .resource_ptr::<T>()
            .unwrap_or_else(|| missing_resource::<T>(run.name));
        // SAFETY: declared write: nothing else touches `T` while the system runs.
        let value = unsafe { &mut *ptr };
        ResMut { value }
    }
}

// SAFETY: writes `T`, as declared.
unsafe impl<T: Send + Sync + 'static> SystemParam for Option<ResMut<'_, T>> {
    type State = ();
    type Item<'w, 's> = Option<ResMut<'w, T>>;

    fn access(access: &mut SystemAccess) {
        ResMut::<T>::access(access);
    }

    fn init_state(_: &mut World) {}

    unsafe fn get<'w>(_: &mut (), run: &SystemRun<'w>) -> Option<ResMut<'w, T>> {
        run.world.resource_ptr::<T>().map(|p| {
            // SAFETY: as for `ResMut`.
            let value = unsafe { &mut *p };
            ResMut { value }
        })
    }
}

// --- Local, Commands, World, jobs ---

/// A system parameter holding data of the system's own, kept between runs; starts as
/// `T::default()`.
pub struct Local<'s, T> {
    value: &'s mut T,
}

impl<T> Deref for Local<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.value
    }
}

impl<T> DerefMut for Local<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.value
    }
}

// SAFETY: touches nothing in the world.
unsafe impl<T: Default + Send + 'static> SystemParam for Local<'_, T> {
    type State = T;
    type Item<'w, 's> = Local<'s, T>;

    fn access(_: &mut SystemAccess) {}

    fn init_state(_: &mut World) -> T {
        T::default()
    }

    unsafe fn get<'w, 's>(state: &'s mut T, _: &SystemRun<'w>) -> Local<'s, T> {
        Local { value: state }
    }
}

// SAFETY: as above.
unsafe impl<T: Default + Send + 'static> ReadOnlySystemParam for Local<'_, T> {}

// SAFETY: touches nothing while the system runs; `apply` has the whole world.
unsafe impl SystemParam for Commands<'_, '_> {
    type State = CommandQueue;
    type Item<'w, 's> = Commands<'w, 's>;

    fn access(_: &mut SystemAccess) {}

    fn init_state(_: &mut World) -> CommandQueue {
        CommandQueue::new()
    }

    unsafe fn get<'w, 's>(state: &'s mut CommandQueue, run: &SystemRun<'w>) -> Commands<'w, 's> {
        Commands::from_entities(state, &run.world.entities, run.world.id())
    }

    fn apply(state: &mut CommandQueue, world: &mut World) {
        world.apply(state);
    }
}

// SAFETY: reads everything, as declared.
unsafe impl SystemParam for &World {
    type State = ();
    type Item<'w, 's> = &'w World;

    fn access(access: &mut SystemAccess) {
        access.add(&access_of(|a| {
            a.read_all();
        }));
    }

    fn init_state(_: &mut World) {}

    unsafe fn get<'w>(_: &mut (), run: &SystemRun<'w>) -> &'w World {
        run.world
    }
}

// SAFETY: as above.
unsafe impl ReadOnlySystemParam for &World {}

// SAFETY: touches nothing in the world.
unsafe impl SystemParam for &JobSystem {
    type State = ();
    type Item<'w, 's> = &'w JobSystem;

    fn access(_: &mut SystemAccess) {}

    fn init_state(_: &mut World) {}

    unsafe fn get<'w>(_: &mut (), run: &SystemRun<'w>) -> &'w JobSystem {
        run.jobs
    }
}

// SAFETY: as above.
unsafe impl ReadOnlySystemParam for &JobSystem {}

// --- Events and removals ---

// SAFETY: writes `Events<E>`, as declared.
unsafe impl<E: Event> SystemParam for EventWriter<'_, E> {
    type State = ();
    type Item<'w, 's> = EventWriter<'w, E>;

    fn access(access: &mut SystemAccess) {
        access.add(&access_of(|a| {
            a.write::<Events<E>>();
        }));
    }

    fn init_state(_: &mut World) {}

    unsafe fn get<'w>(_: &mut (), run: &SystemRun<'w>) -> EventWriter<'w, E> {
        let ptr = run
            .world
            .resource_ptr::<Events<E>>()
            .unwrap_or_else(|| panic!("system `{}`: {}", run.name, missing_events::<E>()));
        // SAFETY: declared write of the resource.
        let events = unsafe { &mut *ptr };
        EventWriter { events }
    }
}

// SAFETY: reads `Events<E>`, as declared; the cursor is the system's.
unsafe impl<E: Event> SystemParam for EventReader<'_, '_, E> {
    type State = usize;
    type Item<'w, 's> = EventReader<'w, 's, E>;

    fn access(access: &mut SystemAccess) {
        access.add(&access_of(|a| {
            a.read::<Events<E>>();
        }));
    }

    fn init_state(_: &mut World) -> usize {
        0
    }

    unsafe fn get<'w, 's>(cursor: &'s mut usize, run: &SystemRun<'w>) -> EventReader<'w, 's, E> {
        let ptr = run
            .world
            .resource_ptr::<Events<E>>()
            .unwrap_or_else(|| panic!("system `{}`: {}", run.name, missing_events::<E>()));
        // SAFETY: declared read of the resource.
        let events = unsafe { &*ptr };
        EventReader { events, cursor }
    }
}

// SAFETY: as above.
unsafe impl<E: Event> ReadOnlySystemParam for EventReader<'_, '_, E> {}

/// A system parameter listing the entities whose `T` was removed (or that were despawned with
/// one) since the system last ran; see [`World::removed_since`]. It reads `T`.
pub struct RemovedComponents<'w, T> {
    world: &'w World,
    change: ChangeTicks,
    _marker: PhantomData<fn() -> T>,
}

impl<T: Component> RemovedComponents<'_, T> {
    pub fn iter(&self) -> impl Iterator<Item = Entity> + '_ {
        self.world
            .removed_since::<T>(self.change.last_run, self.change.this_run)
    }
}

// SAFETY: reads the removal records of `T`, declared as a read of `T`.
unsafe impl<T: Component> SystemParam for RemovedComponents<'_, T> {
    type State = ();
    type Item<'w, 's> = RemovedComponents<'w, T>;

    fn access(access: &mut SystemAccess) {
        access.add(&access_of(|a| {
            a.read::<T>();
        }));
    }

    fn init_state(_: &mut World) {}

    unsafe fn get<'w>(_: &mut (), run: &SystemRun<'w>) -> RemovedComponents<'w, T> {
        RemovedComponents {
            world: run.world,
            change: run.change,
            _marker: PhantomData,
        }
    }
}

// SAFETY: as above.
unsafe impl<T: Component> ReadOnlySystemParam for RemovedComponents<'_, T> {}

// --- Tuples ---

macro_rules! tuple_param {
    ($($P:ident),*) => {
        // SAFETY: the union of the members' accesses, checked for conflicts as they are added.
        #[allow(non_snake_case, unused_variables, unused_unsafe, clippy::unused_unit)]
        unsafe impl<$($P: SystemParam),*> SystemParam for ($($P,)*) {
            type State = ($($P::State,)*);
            type Item<'w, 's> = ($($P::Item<'w, 's>,)*);

            fn access(access: &mut SystemAccess) {
                $($P::access(access);)*
            }

            fn init_state(world: &mut World) -> Self::State {
                ($($P::init_state(world),)*)
            }

            unsafe fn get<'w, 's>(
                state: &'s mut Self::State,
                run: &SystemRun<'w>,
            ) -> Self::Item<'w, 's> {
                let ($($P,)*) = state;
                // SAFETY: forwarded from the caller; the members do not conflict.
                unsafe { ($($P::get($P, run),)*) }
            }

            fn apply(state: &mut Self::State, world: &mut World) {
                let ($($P,)*) = state;
                $($P::apply($P, world);)*
            }
        }

        // SAFETY: every member only reads.
        unsafe impl<$($P: ReadOnlySystemParam),*> ReadOnlySystemParam for ($($P,)*) {}
    };
}

tuple_param!();
tuple_param!(A);
tuple_param!(A, B);
tuple_param!(A, B, C);
tuple_param!(A, B, C, D);
tuple_param!(A, B, C, D, E);
tuple_param!(A, B, C, D, E, F);
tuple_param!(A, B, C, D, E, F, G);
tuple_param!(A, B, C, D, E, F, G, H);
tuple_param!(A, B, C, D, E, F, G, H, I);
tuple_param!(A, B, C, D, E, F, G, H, I, J);
tuple_param!(A, B, C, D, E, F, G, H, I, J, K);
tuple_param!(A, B, C, D, E, F, G, H, I, J, K, L);

// --- Functions ---

/// A function whose parameters are all [`SystemParam`]s. `Marker` is `fn(Params) -> Out`, which
/// tells the implementations for each arity apart.
pub trait SystemParamFunction<Marker>: Send + 'static {
    type Param: SystemParam;
    type Out;

    fn run(&mut self, param: SystemParamItem<'_, '_, Self::Param>) -> Self::Out;
}

macro_rules! impl_system_function {
    ($($P:ident),*) => {
        #[allow(non_snake_case)]
        impl<Out, Func, $($P: SystemParam),*> SystemParamFunction<fn($($P,)*) -> Out> for Func
        where
            Func: Send + 'static,
            for<'a> &'a mut Func: FnMut($($P),*) -> Out + FnMut($(SystemParamItem<$P>),*) -> Out,
        {
            type Param = ($($P,)*);
            type Out = Out;

            #[inline]
            fn run(&mut self, param: SystemParamItem<'_, '_, ($($P,)*)>) -> Out {
                // Calling through a generic function pins the second `FnMut` bound.
                #[allow(clippy::too_many_arguments)]
                fn call<Out, $($P),*>(mut f: impl FnMut($($P),*) -> Out, $($P: $P),*) -> Out {
                    f($($P),*)
                }
                let ($($P,)*) = param;
                call(self, $($P),*)
            }
        }
    };
}

impl_system_function!();
impl_system_function!(A);
impl_system_function!(A, B);
impl_system_function!(A, B, C);
impl_system_function!(A, B, C, D);
impl_system_function!(A, B, C, D, E);
impl_system_function!(A, B, C, D, E, F);
impl_system_function!(A, B, C, D, E, F, G);
impl_system_function!(A, B, C, D, E, F, G, H);
impl_system_function!(A, B, C, D, E, F, G, H, I);
impl_system_function!(A, B, C, D, E, F, G, H, I, J);
impl_system_function!(A, B, C, D, E, F, G, H, I, J, K);
impl_system_function!(A, B, C, D, E, F, G, H, I, J, K, L);

/// The last path segment of a function's type name, without generic arguments: `movement` for
/// `game::movement`, `update_events` for `update_events<Hit>`.
pub(crate) fn short_name<F>() -> &'static str {
    let full = core::any::type_name::<F>();
    let path = full.find('<').map_or(full, |i| &full[..i]);
    path.rsplit("::").next().unwrap_or(path)
}

/// What the schedule drives: a shared system, an exclusive one, or a run condition.
pub(crate) trait Runner: Send {
    fn initialize(&mut self, world: &mut World);
    fn is_exclusive(&self) -> bool;
    /// # Safety
    /// For the call, nothing running alongside conflicts with the system's access, which is
    /// checked against its declaration.
    unsafe fn run_shared(&mut self, run: &SystemRun<'_>, access: &Access);
    fn run_exclusive(&mut self, world: &mut World, jobs: &JobSystem);
    fn apply(&mut self, world: &mut World);
}

pub(crate) struct FunctionSystem<Marker, F: SystemParamFunction<Marker>> {
    func: F,
    state: Option<<F::Param as SystemParam>::State>,
    _marker: PhantomData<fn() -> Marker>,
}

impl<Marker: 'static, F> Runner for FunctionSystem<Marker, F>
where
    F: SystemParamFunction<Marker, Out = ()>,
{
    fn initialize(&mut self, world: &mut World) {
        if self.state.is_none() {
            self.state = Some(F::Param::init_state(world));
        }
    }

    fn is_exclusive(&self) -> bool {
        false
    }

    unsafe fn run_shared(&mut self, run: &SystemRun<'_>, _: &Access) {
        let state = self.state.as_mut().expect("initialized before running");
        // SAFETY: the parameters' access is the system's, which the caller upholds.
        let param = unsafe { F::Param::get(state, run) };
        self.func.run(param);
    }

    fn run_exclusive(&mut self, _: &mut World, _: &JobSystem) {
        unreachable!("function systems with parameters are shared")
    }

    fn apply(&mut self, world: &mut World) {
        if let Some(state) = &mut self.state {
            F::Param::apply(state, world);
        }
    }
}

pub(crate) struct ExclusiveFunction<F>(pub(crate) F);

impl<F: FnMut(&mut World) + Send + 'static> Runner for ExclusiveFunction<F> {
    fn initialize(&mut self, _: &mut World) {}

    fn is_exclusive(&self) -> bool {
        true
    }

    unsafe fn run_shared(&mut self, _: &SystemRun<'_>, _: &Access) {
        unreachable!("exclusive systems run alone")
    }

    fn run_exclusive(&mut self, world: &mut World, _: &JobSystem) {
        (self.0)(world);
    }

    fn apply(&mut self, _: &mut World) {}
}

/// Builds a system's runner and access from a function.
pub(crate) fn function_system<Marker: 'static, F>(
    func: F,
) -> (&'static str, Access, Box<dyn Runner>)
where
    F: SystemParamFunction<Marker, Out = ()>,
{
    let name = short_name::<F>();
    let mut access = SystemAccess::new(name);
    F::Param::access(&mut access);
    let runner = FunctionSystem {
        func,
        state: None,
        _marker: PhantomData::<fn() -> Marker>,
    };
    (name, access.into_access(), Box::new(runner))
}

// --- Run conditions ---

/// A run condition: a function of read-only parameters returning whether to run, such as
/// `|paused: Res<Paused>| !paused.0` or `fn(&World) -> bool`. Conditions are evaluated on the
/// thread running the schedule, before their system's wave starts, while no system runs.
pub struct Condition {
    pub(crate) name: &'static str,
    pub(crate) runner: Box<dyn ConditionRunner>,
    /// When the condition was last evaluated, for its change detection.
    pub(crate) last_run: Tick,
}

pub(crate) trait ConditionRunner: Send {
    fn initialize(&mut self, world: &mut World);
    fn evaluate(&mut self, run: &SystemRun<'_>) -> bool;
}

struct FunctionCondition<Marker, F: SystemParamFunction<Marker>> {
    func: F,
    state: Option<<F::Param as SystemParam>::State>,
    _marker: PhantomData<fn() -> Marker>,
}

impl<Marker: 'static, F> ConditionRunner for FunctionCondition<Marker, F>
where
    F: SystemParamFunction<Marker, Out = bool>,
    F::Param: ReadOnlySystemParam,
{
    fn initialize(&mut self, world: &mut World) {
        if self.state.is_none() {
            self.state = Some(F::Param::init_state(world));
        }
    }

    fn evaluate(&mut self, run: &SystemRun<'_>) -> bool {
        let state = self.state.as_mut().expect("initialized before evaluating");
        // SAFETY: read-only parameters, evaluated while no system runs.
        let param = unsafe { F::Param::get(state, run) };
        self.func.run(param)
    }
}

/// Anything usable as a [`Condition`].
pub trait IntoCondition<Marker> {
    fn into_condition(self) -> Condition;
}

impl IntoCondition<()> for Condition {
    fn into_condition(self) -> Condition {
        self
    }
}

impl<Marker: 'static, F> IntoCondition<(Condition, Marker)> for F
where
    F: SystemParamFunction<Marker, Out = bool>,
    F::Param: ReadOnlySystemParam,
{
    fn into_condition(self) -> Condition {
        let name = short_name::<F>();
        // Rejects conflicting parameters like a system would.
        F::Param::access(&mut SystemAccess::new(name));
        Condition {
            name,
            runner: Box::new(FunctionCondition {
                func: self,
                state: None,
                _marker: PhantomData::<fn() -> Marker>,
            }),
            last_run: Tick::default(),
        }
    }
}

/// A condition that holds while resource `R` exists: `.run_if(resource_exists::<Paused>)`.
pub fn resource_exists<R: Send + Sync + 'static>(world: &World) -> bool {
    world.has_resource::<R>()
}

/// The negation of a condition: `.run_if(not(resource_exists::<Paused>))`.
pub fn not<M>(condition: impl IntoCondition<M>) -> Condition {
    let mut inner = condition.into_condition();
    struct Not(Box<dyn ConditionRunner>);
    impl ConditionRunner for Not {
        fn initialize(&mut self, world: &mut World) {
            self.0.initialize(world);
        }

        fn evaluate(&mut self, run: &SystemRun<'_>) -> bool {
            !self.0.evaluate(run)
        }
    }
    inner.runner = Box::new(Not(inner.runner));
    inner
}
