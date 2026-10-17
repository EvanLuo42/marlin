//! Systems and their automatically parallel schedule.
//!
//! A system says which component and resource types it reads and writes: closure systems declare
//! it ([`System::reads`] / [`System::writes`]), function systems derive it from their parameters
//! ([`crate::system`]). Every access a system makes is checked against that at run time, which is
//! what makes running systems in parallel through a shared world sound.
//!
//! **Order.** Systems may be grouped into named sets ([`System::in_set`],
//! [`Schedule::configure_set`]) and ordered explicitly with [`before`](System::before) /
//! [`after`](System::after), naming a system or a set (a name stands for every system with that
//! name and every member of the set with that name, sets nested in it included). The schedule
//! first sorts the systems topologically by those constraints, taking the earliest-added ready
//! system at each step, so without constraints the order is the order of addition; a cycle panics
//! with the systems on it. It then places each system, in that order, in the earliest *wave*
//! after every earlier system it is ordered after or conflicts with. Each wave's systems run in
//! parallel on the job system, and the commands they recorded are applied after the wave in the
//! order the systems were added. The waves depend only on the systems and constraints, so they
//! are deterministic.
//!
//! **Conditions.** [`System::run_if`] and [`SetConfig::run_if`] take functions of read-only
//! parameters returning `bool`. All of a system's conditions, and those of every set it is in
//! (each set's evaluated once per schedule run), are evaluated just before its wave, while no
//! system runs; the system runs only if all hold.

use alloc::boxed::Box;
use alloc::collections::BinaryHeap;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Reverse;

use marlin_containers::{HashMap, HashMapExt};
use marlin_jobs::JobSystem;

use crate::change::{ChangeTicks, Mut, Tick};
use crate::command::{CommandQueue, Commands};
use crate::component::{Component, ComponentId};
use crate::dynamic::{ComponentMut, ComponentPtr, DynamicQuery, DynamicRow};
use crate::entity::Entity;
use crate::event::{Event, Events, missing_events};
use crate::hierarchy::{Children, Parent};
use crate::query::{
    Access, AccessKey, QueryData, QueryFilter, QueryIter, QueryState, ReadOnlyQueryData,
    query_access,
};
use crate::system::{
    Condition, ExclusiveFunction, IntoCondition, Runner, SystemParamFunction, SystemRun,
    function_system,
};
use crate::world::World;

/// A system's view of the world: queries, resources and components it declared, nothing else.
pub struct SystemWorld<'w> {
    world: &'w World,
    access: &'w Access,
    name: &'static str,
    jobs: &'w JobSystem,
    /// Changes since the system last ran.
    change: ChangeTicks,
}

impl<'w> SystemWorld<'w> {
    #[track_caller]
    fn check(&self, needed: &Access) {
        if let Some(name) = needed.first_outside(self.access) {
            panic!(
                "system `{}` accesses `{name}` without declaring it",
                self.name
            );
        }
    }

    #[track_caller]
    fn check_query<Q: QueryData, F: QueryFilter>(&self) {
        self.check(&query_access::<Q, F>());
    }

    #[track_caller]
    fn check_id(&self, id: ComponentId, write: bool) {
        let info = self.world.components.info(id);
        let mut access = Access::new();
        if write {
            access.write_key(info.access_key(), info.name());
        } else {
            access.read_key(info.access_key(), info.name());
        }
        self.check(&access);
    }

    /// The ticks this run compares against: `last_run` is the system's previous run.
    pub fn change_ticks(&self) -> ChangeTicks {
        self.change
    }

    /// A read-only query; several may be alive at once.
    #[track_caller]
    pub fn query<Q: ReadOnlyQueryData>(&self) -> QueryIter<'_, Q> {
        self.check_query::<Q, ()>();
        // SAFETY: the access is declared, and the schedule never runs a writer of these types
        // alongside this system.
        unsafe { QueryIter::new(self.world, self.change) }
    }

    #[track_caller]
    pub fn query_filtered<Q: ReadOnlyQueryData, F: QueryFilter>(&self) -> QueryIter<'_, Q, F> {
        self.check_query::<Q, F>();
        // SAFETY: as above.
        unsafe { QueryIter::new(self.world, self.change) }
    }

    /// A query that may write; exclusive within the system.
    #[track_caller]
    pub fn query_mut<Q: QueryData>(&mut self) -> QueryIter<'_, Q> {
        self.check_query::<Q, ()>();
        // SAFETY: declared access; the `&mut self` borrow keeps the system's other accesses out.
        unsafe { QueryIter::new(self.world, self.change) }
    }

    #[track_caller]
    pub fn query_mut_filtered<Q: QueryData, F: QueryFilter>(&mut self) -> QueryIter<'_, Q, F> {
        self.check_query::<Q, F>();
        // SAFETY: as above.
        unsafe { QueryIter::new(self.world, self.change) }
    }

    /// A read-only query through a cached [`QueryState`] the system keeps.
    #[track_caller]
    pub fn query_cached<'s, Q: ReadOnlyQueryData, F: QueryFilter>(
        &'s self,
        state: &'s mut QueryState<Q, F>,
    ) -> QueryIter<'s, Q, F> {
        self.check_query::<Q, F>();
        state.update(self.world);
        // SAFETY: as for `query`.
        unsafe { state.iter_unchecked(self.world, self.change) }
    }

    /// A query that may write, through a cached [`QueryState`].
    #[track_caller]
    pub fn query_cached_mut<'s, Q: QueryData, F: QueryFilter>(
        &'s mut self,
        state: &'s mut QueryState<Q, F>,
    ) -> QueryIter<'s, Q, F> {
        self.check_query::<Q, F>();
        state.update(self.world);
        // SAFETY: as for `query_mut`.
        unsafe { state.iter_unchecked(self.world, self.change) }
    }

    /// [`World::par_for_each`] within the system's declared access.
    #[track_caller]
    pub fn par_for_each<Q, F>(&mut self, batch: usize, f: F)
    where
        Q: QueryData,
        F: for<'a> Fn(Q::Item<'a>) + Sync,
    {
        self.par_for_each_filtered::<Q, (), F>(batch, f);
    }

    #[track_caller]
    pub fn par_for_each_filtered<Q, Filter, F>(&mut self, batch: usize, f: F)
    where
        Q: QueryData,
        Filter: QueryFilter,
        F: for<'a> Fn(Q::Item<'a>) + Sync,
    {
        self.check_query::<Q, Filter>();
        // SAFETY: as for `query_mut`.
        unsafe {
            self.world
                .par_for_each_unchecked::<Q, Filter, F>(self.jobs, batch, self.change, &f)
        }
    }

    /// Runs a [`DynamicQuery`], whose reads and writes must be declared
    /// ([`System::with_access`]).
    #[track_caller]
    pub fn dynamic_for_each(&mut self, query: &mut DynamicQuery, f: impl FnMut(DynamicRow<'_>)) {
        self.check(query.access());
        query.update(self.world);
        // SAFETY: as for `query_mut`.
        unsafe { query.for_each_unchecked(self.world, self.change, f) }
    }

    /// Entities whose `T` was removed (or despawned) since the system last ran.
    #[track_caller]
    pub fn removed<T: Component>(&self) -> impl Iterator<Item = Entity> + '_ {
        self.check(Access::new().read::<T>());
        self.world
            .removed_since::<T>(self.change.last_run, self.change.this_run)
    }

    #[track_caller]
    pub fn resource<R: Send + Sync + 'static>(&self) -> Option<&R> {
        self.check(Access::new().read::<R>());
        // SAFETY: declared read; no writer of `R` runs alongside.
        self.world.resource_ptr::<R>().map(|p| unsafe { &*p })
    }

    #[track_caller]
    pub fn resource_mut<R: Send + Sync + 'static>(&mut self) -> Option<&mut R> {
        self.check(Access::new().write::<R>());
        // SAFETY: declared write, so nothing else touches `R`; `&mut self` keeps it unique here.
        self.world.resource_ptr::<R>().map(|p| unsafe { &mut *p })
    }

    /// Sends an event; needs a declared write of `Events<E>`.
    ///
    /// # Panics
    /// If `E` was not added with [`World::add_event`].
    #[track_caller]
    pub fn send_event<E: Event>(&mut self, event: E) {
        self.resource_mut::<Events<E>>()
            .unwrap_or_else(|| panic!("{}", missing_events::<E>()))
            .send(event);
    }

    #[track_caller]
    pub fn get<T: Component>(&self, entity: Entity) -> Option<&T> {
        self.check(Access::new().read::<T>());
        // SAFETY: declared read.
        self.world.get(entity)
    }

    /// The entity's `T` for writing; writes through the [`Mut`] mark it changed.
    #[track_caller]
    pub fn get_mut<T: Component>(&mut self, entity: Entity) -> Option<Mut<'_, T>> {
        self.check(Access::new().write::<T>());
        let id = self.world.component_id::<T>()?;
        let (value, ticks) = self.world.component_cells(entity, id)?;
        // SAFETY: declared write; `&mut self` keeps it unique within the system, and no other
        // system touches `T` meanwhile.
        Some(unsafe { Mut::new(&mut *value.cast::<T>(), &mut *ticks, self.change) })
    }

    /// The entity's component `id`; needs it declared as read.
    #[track_caller]
    pub fn get_by_id(&self, entity: Entity, id: ComponentId) -> Option<ComponentPtr<'_>> {
        self.check_id(id, false);
        self.world.get_by_id(entity, id)
    }

    /// The entity's component `id` for writing; needs it declared as written.
    #[track_caller]
    pub fn get_mut_by_id(&mut self, entity: Entity, id: ComponentId) -> Option<ComponentMut<'_>> {
        self.check_id(id, true);
        let info = self.world.components.info(id);
        let (ptr, ticks) = self.world.component_cells(entity, id)?;
        // SAFETY: declared write, so the row is this system's alone.
        Some(unsafe { ComponentMut::from_raw(ptr, info, &mut *ticks, self.change.this_run) })
    }

    /// The entity's parent; needs `Parent` declared as read.
    #[track_caller]
    pub fn parent(&self, entity: Entity) -> Option<Entity> {
        self.get::<Parent>(entity).map(Parent::get)
    }

    /// The entity's children; needs `Children` declared as read.
    #[track_caller]
    pub fn children(&self, entity: Entity) -> &[Entity] {
        self.get::<Children>(entity).map_or(&[], |c| c.as_slice())
    }

    pub fn is_alive(&self, entity: Entity) -> bool {
        self.world.is_alive(entity)
    }

    /// For nested parallelism inside the system.
    pub fn jobs(&self) -> &JobSystem {
        self.jobs
    }
}

/// What a closure system receives each run.
pub struct SystemContext<'w> {
    pub world: SystemWorld<'w>,
    /// Applied after the system's wave, in system order.
    pub commands: Commands<'w, 'w>,
}

type SystemFn = Box<dyn FnMut(&mut SystemContext<'_>) + Send>;
type ExclusiveFn = Box<dyn FnMut(&mut World, &JobSystem) + Send>;

struct ClosureSystem {
    run: SystemFn,
    queue: CommandQueue,
}

impl Runner for ClosureSystem {
    fn initialize(&mut self, _: &mut World) {}

    fn is_exclusive(&self) -> bool {
        false
    }

    unsafe fn run_shared(&mut self, run: &SystemRun<'_>, access: &Access) {
        let mut context = SystemContext {
            world: SystemWorld {
                world: run.world,
                access,
                name: run.name,
                jobs: run.jobs,
                change: run.change,
            },
            commands: Commands::from_entities(&mut self.queue, &run.world.entities, run.world.id()),
        };
        (self.run)(&mut context);
    }

    fn run_exclusive(&mut self, _: &mut World, _: &JobSystem) {
        unreachable!("shared systems run in waves")
    }

    fn apply(&mut self, world: &mut World) {
        world.apply(&mut self.queue);
    }
}

struct ExclusiveClosure(ExclusiveFn);

impl Runner for ExclusiveClosure {
    fn initialize(&mut self, _: &mut World) {}

    fn is_exclusive(&self) -> bool {
        true
    }

    unsafe fn run_shared(&mut self, _: &SystemRun<'_>, _: &Access) {
        unreachable!("exclusive systems run alone")
    }

    fn run_exclusive(&mut self, world: &mut World, jobs: &JobSystem) {
        (self.0)(world, jobs);
    }

    fn apply(&mut self, _: &mut World) {}
}

pub struct System {
    name: &'static str,
    access: Access,
    runner: Box<dyn Runner>,
    sets: Vec<&'static str>,
    before: Vec<&'static str>,
    after: Vec<&'static str>,
    conditions: Vec<Condition>,
}

impl System {
    fn from_runner(name: &'static str, access: Access, runner: Box<dyn Runner>) -> Self {
        Self {
            name,
            access,
            runner,
            sets: Vec::new(),
            before: Vec::new(),
            after: Vec::new(),
            conditions: Vec::new(),
        }
    }

    /// A system that runs in parallel with others it does not conflict with. Declare what it
    /// touches with [`reads`](Self::reads) / [`writes`](Self::writes).
    pub fn new(
        name: &'static str,
        run: impl FnMut(&mut SystemContext<'_>) + Send + 'static,
    ) -> Self {
        Self::from_runner(
            name,
            Access::new(),
            Box::new(ClosureSystem {
                run: Box::new(run),
                queue: CommandQueue::new(),
            }),
        )
    }

    /// A system with `&mut World`, running alone between the systems ordered before and after
    /// it.
    pub fn exclusive(
        name: &'static str,
        run: impl FnMut(&mut World, &JobSystem) + Send + 'static,
    ) -> Self {
        Self::from_runner(
            name,
            Access::new(),
            Box::new(ExclusiveClosure(Box::new(run))),
        )
    }

    /// Declares a component or resource type the system reads.
    pub fn reads<T: 'static>(mut self) -> Self {
        self.access.read::<T>();
        self
    }

    /// Declares a component or resource type the system writes (and may read).
    pub fn writes<T: 'static>(mut self) -> Self {
        self.access.write::<T>();
        self
    }

    /// Declares a read by access key, e.g. of a component registered without a Rust type
    /// ([`ComponentInfo::access_key`](crate::ComponentInfo::access_key)).
    pub fn reads_key(mut self, key: AccessKey, name: &'static str) -> Self {
        self.access.read_key(key, name);
        self
    }

    pub fn writes_key(mut self, key: AccessKey, name: &'static str) -> Self {
        self.access.write_key(key, name);
        self
    }

    /// Declares everything `access` reads and writes, e.g. a [`DynamicQuery`]'s.
    pub fn with_access(mut self, access: &Access) -> Self {
        self.access.extend(access);
        self
    }

    /// Renames the system; function systems are named after their function.
    pub fn named(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    /// Adds the system to a set (see the module docs).
    pub fn in_set(mut self, set: &'static str) -> Self {
        self.sets.push(set);
        self
    }

    /// Runs the system before every system and set member named `label`.
    pub fn before(mut self, label: &'static str) -> Self {
        self.before.push(label);
        self
    }

    /// Runs the system after every system and set member named `label`.
    pub fn after(mut self, label: &'static str) -> Self {
        self.after.push(label);
        self
    }

    /// Runs the system only when `condition` holds.
    pub fn run_if<M>(mut self, condition: impl IntoCondition<M>) -> Self {
        self.conditions.push(condition.into_condition());
        self
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// What the system declared or derived.
    pub fn access(&self) -> &Access {
        &self.access
    }

    pub fn is_exclusive(&self) -> bool {
        self.runner.is_exclusive()
    }

    fn conflicts_with(&self, other: &Self) -> bool {
        self.is_exclusive() || other.is_exclusive() || self.access.conflicts_with(&other.access)
    }
}

/// Anything usable as a [`System`]: a `System`, a function of [`SystemParam`]s, or a
/// `FnMut(&mut World)` (exclusive). The configuration methods turn it into a `System` first.
///
/// [`SystemParam`]: crate::SystemParam
pub trait IntoSystem<Marker>: Sized {
    fn into_system(self) -> System;

    fn named(self, name: &'static str) -> System {
        self.into_system().named(name)
    }

    fn in_set(self, set: &'static str) -> System {
        self.into_system().in_set(set)
    }

    fn before(self, label: &'static str) -> System {
        self.into_system().before(label)
    }

    fn after(self, label: &'static str) -> System {
        self.into_system().after(label)
    }

    fn run_if<M>(self, condition: impl IntoCondition<M>) -> System {
        self.into_system().run_if(condition)
    }
}

impl IntoSystem<()> for System {
    fn into_system(self) -> System {
        self
    }
}

#[doc(hidden)]
pub struct FunctionMarker;

impl<Marker: 'static, F> IntoSystem<(FunctionMarker, Marker)> for F
where
    F: SystemParamFunction<Marker, Out = ()>,
{
    fn into_system(self) -> System {
        let (name, access, runner) = function_system(self);
        System::from_runner(name, access, runner)
    }
}

#[doc(hidden)]
pub struct ExclusiveMarker;

impl<F> IntoSystem<ExclusiveMarker> for F
where
    F: FnMut(&mut World) + Send + 'static,
{
    fn into_system(self) -> System {
        let name = crate::system::short_name::<F>();
        System::from_runner(name, Access::new(), Box::new(ExclusiveFunction(self)))
    }
}

/// A named group of systems: its ordering and conditions apply to every member.
pub struct SetConfig {
    name: &'static str,
    parents: Vec<&'static str>,
    before: Vec<&'static str>,
    after: Vec<&'static str>,
    conditions: Vec<Condition>,
    initialized: bool,
}

impl SetConfig {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            parents: Vec::new(),
            before: Vec::new(),
            after: Vec::new(),
            conditions: Vec::new(),
            initialized: false,
        }
    }

    /// Nests this set in `set`: its members are members of `set` too.
    pub fn in_set(&mut self, set: &'static str) -> &mut Self {
        self.parents.push(set);
        self
    }

    pub fn before(&mut self, label: &'static str) -> &mut Self {
        self.before.push(label);
        self
    }

    pub fn after(&mut self, label: &'static str) -> &mut Self {
        self.after.push(label);
        self
    }

    /// Runs the set's members only when `condition` holds; evaluated once per schedule run.
    pub fn run_if<M>(&mut self, condition: impl IntoCondition<M>) -> &mut Self {
        self.conditions.push(condition.into_condition());
        self
    }
}

struct Entry {
    system: System,
    /// The tick of the system's previous run (0 before the first: everything reads as added).
    last_run: Tick,
    initialized: bool,
}

/// The order a schedule runs in, derived from its systems and sets.
struct Plan {
    /// System indices per wave, ascending.
    waves: Vec<Vec<usize>>,
    /// Per system: the sets it is in, nested ones included, by index into `sets`.
    system_sets: Vec<Vec<usize>>,
}

#[derive(Default)]
pub struct Schedule {
    systems: Vec<Entry>,
    sets: Vec<SetConfig>,
    /// Rebuilt after systems or sets change.
    plan: Option<Plan>,
}

impl Schedule {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a [`System`], a function system, or an exclusive `FnMut(&mut World)`.
    pub fn add_system<M>(&mut self, system: impl IntoSystem<M>) -> &mut Self {
        self.systems.push(Entry {
            system: system.into_system(),
            last_run: Tick::default(),
            initialized: false,
        });
        self.plan = None;
        self
    }

    /// The configuration of set `name`, created on first use.
    pub fn configure_set(&mut self, name: &'static str) -> &mut SetConfig {
        self.plan = None;
        let index = match self.sets.iter().position(|s| s.name == name) {
            Some(i) => i,
            None => {
                self.sets.push(SetConfig::new(name));
                self.sets.len() - 1
            }
        };
        &mut self.sets[index]
    }

    /// Names of the systems in each wave, for inspection.
    ///
    /// # Panics
    /// As for [`run`](Self::run): on an ordering cycle or an unknown label.
    pub fn wave_names(&mut self) -> Vec<Vec<&'static str>> {
        self.plan();
        let plan = self.plan.as_ref().expect("planned");
        plan.waves
            .iter()
            .map(|w| w.iter().map(|&i| self.systems[i].system.name).collect())
            .collect()
    }

    fn plan(&mut self) -> &Plan {
        if self.plan.is_none() {
            // Systems may name sets nobody configured: those exist implicitly.
            let referenced: Vec<&'static str> = self
                .systems
                .iter()
                .flat_map(|e| e.system.sets.iter().copied())
                .chain(self.sets.iter().flat_map(|s| s.parents.iter().copied()))
                .collect();
            for name in referenced {
                if !self.sets.iter().any(|s| s.name == name) {
                    self.sets.push(SetConfig::new(name));
                }
            }
            self.plan = Some(build_plan(&self.systems, &self.sets));
        }
        self.plan.as_ref().expect("just built")
    }

    /// Runs every system once, wave by wave, applying each wave's commands after it.
    ///
    /// # Panics
    /// If the ordering constraints form a cycle (naming the systems on it), or name something
    /// that is neither a system nor a set.
    pub fn run(&mut self, world: &mut World, jobs: &JobSystem) {
        self.plan();
        let plan = self.plan.take().expect("planned");
        world.flush();
        for entry in &mut self.systems {
            if !entry.initialized {
                entry.system.runner.initialize(world);
                for condition in &mut entry.system.conditions {
                    condition.runner.initialize(world);
                }
                entry.initialized = true;
            }
        }
        for set in &mut self.sets {
            if !set.initialized {
                for condition in &mut set.conditions {
                    condition.runner.initialize(world);
                }
                set.initialized = true;
            }
        }

        let mut set_results: Vec<Option<bool>> = vec![None; self.sets.len()];
        let mut runnable: Vec<usize> = Vec::new();
        for wave in &plan.waves {
            runnable.clear();
            for &i in wave {
                if self.should_run(i, &plan, &mut set_results, world, jobs) {
                    runnable.push(i);
                }
            }
            let Some(&first) = runnable.first() else {
                continue;
            };

            if self.systems[first].system.is_exclusive() {
                debug_assert_eq!(runnable.len(), 1, "exclusive systems run alone");
                let entry = &mut self.systems[first];
                entry.last_run = world.increment_change_tick();
                entry.system.runner.run_exclusive(world, jobs);
                continue;
            }

            let shared: &World = world;
            if let [only] = runnable[..] {
                Self::run_shared(&mut self.systems[only], shared, jobs);
            } else {
                let mut members: Vec<&mut Entry> = Vec::with_capacity(runnable.len());
                let mut next = runnable.iter().copied().peekable();
                for (i, entry) in self.systems.iter_mut().enumerate() {
                    if next.peek() == Some(&i) {
                        next.next();
                        members.push(entry);
                    }
                }
                jobs.scope(|s| {
                    for entry in members {
                        s.spawn(|| Self::run_shared(entry, shared, jobs));
                    }
                });
            }

            // Commands in system order, whatever order the systems finished in.
            for &i in &runnable {
                self.systems[i].system.runner.apply(world);
            }
        }
        self.plan = Some(plan);

        if let Some(now) = world.check_change_ticks() {
            for entry in &mut self.systems {
                entry.last_run.clamp(now);
                for condition in &mut entry.system.conditions {
                    condition.last_run.clamp(now);
                }
            }
            for set in &mut self.sets {
                for condition in &mut set.conditions {
                    condition.last_run.clamp(now);
                }
            }
        }
    }

    /// Evaluates the conditions of system `i` and of its sets (each set once per run).
    fn should_run(
        &mut self,
        i: usize,
        plan: &Plan,
        set_results: &mut [Option<bool>],
        world: &World,
        jobs: &JobSystem,
    ) -> bool {
        let mut run = true;
        for &s in &plan.system_sets[i] {
            let result = match set_results[s] {
                Some(result) => result,
                None => {
                    let result = evaluate_all(&mut self.sets[s].conditions, world, jobs);
                    set_results[s] = Some(result);
                    result
                }
            };
            run &= result;
        }
        run & evaluate_all(&mut self.systems[i].system.conditions, world, jobs)
    }

    fn run_shared(entry: &mut Entry, world: &World, jobs: &JobSystem) {
        let this_run = world.increment_change_tick();
        let run = SystemRun {
            world,
            jobs,
            change: ChangeTicks {
                last_run: entry.last_run,
                this_run,
            },
            name: entry.system.name,
        };
        // SAFETY: the schedule runs this system only alongside systems it does not conflict
        // with; closure systems check each access against the declaration.
        unsafe { entry.system.runner.run_shared(&run, &entry.system.access) };
        entry.last_run = this_run;
    }
}

/// Evaluates every condition (none is skipped, so each sees every run) and whether all hold.
fn evaluate_all(conditions: &mut [Condition], world: &World, jobs: &JobSystem) -> bool {
    let mut all = true;
    for condition in conditions {
        let this_run = world.increment_change_tick();
        let run = SystemRun {
            world,
            jobs,
            change: ChangeTicks {
                last_run: condition.last_run,
                this_run,
            },
            name: condition.name,
        };
        all &= condition.runner.evaluate(&run);
        condition.last_run = this_run;
    }
    all
}

/// Sorts the systems by their ordering constraints and places them in waves.
fn build_plan(systems: &[Entry], sets: &[SetConfig]) -> Plan {
    let set_index: HashMap<&'static str, usize> = {
        let mut map = HashMap::new();
        for (i, set) in sets.iter().enumerate() {
            map.insert(set.name, i);
        }
        map
    };

    // Each set's ancestors, itself included; nesting cycles are an error.
    let mut closure: Vec<Option<Vec<usize>>> = vec![None; sets.len()];
    for s in 0..sets.len() {
        set_closure(s, sets, &set_index, &mut closure, &mut Vec::new());
    }
    let system_sets: Vec<Vec<usize>> = systems
        .iter()
        .map(|e| {
            let mut all: Vec<usize> = e
                .system
                .sets
                .iter()
                .flat_map(|name| closure[set_index[name]].iter().flatten().copied())
                .collect();
            all.sort_unstable();
            all.dedup();
            all
        })
        .collect();

    // What each label stands for.
    let members = |label: &'static str| -> Vec<usize> {
        let set = set_index.get(label).copied();
        let found: Vec<usize> = (0..systems.len())
            .filter(|&i| {
                systems[i].system.name == label || set.is_some_and(|s| system_sets[i].contains(&s))
            })
            .collect();
        assert!(
            set.is_some() || !found.is_empty(),
            "ordering label `{label}` names no system or set"
        );
        found
    };

    let n = systems.len();
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut add = |from: usize, to: usize| {
        if from != to {
            edges[from].push(to);
        }
    };
    for (i, entry) in systems.iter().enumerate() {
        for &label in &entry.system.before {
            for j in members(label) {
                add(i, j);
            }
        }
        for &label in &entry.system.after {
            for j in members(label) {
                add(j, i);
            }
        }
    }
    for set in sets {
        if set.before.is_empty() && set.after.is_empty() {
            continue;
        }
        let own = members(set.name);
        for &label in &set.before {
            let others = members(label);
            for &m in &own {
                for &j in &others {
                    add(m, j);
                }
            }
        }
        for &label in &set.after {
            let others = members(label);
            for &m in &own {
                for &j in &others {
                    add(j, m);
                }
            }
        }
    }
    for list in &mut edges {
        list.sort_unstable();
        list.dedup();
    }

    // Kahn's algorithm, earliest-added ready system first.
    let mut indegree = vec![0usize; n];
    for list in &edges {
        for &to in list {
            indegree[to] += 1;
        }
    }
    let mut ready: BinaryHeap<Reverse<usize>> =
        (0..n).filter(|&i| indegree[i] == 0).map(Reverse).collect();
    let mut order = Vec::with_capacity(n);
    while let Some(Reverse(i)) = ready.pop() {
        order.push(i);
        for &to in &edges[i] {
            indegree[to] -= 1;
            if indegree[to] == 0 {
                ready.push(Reverse(to));
            }
        }
    }
    if order.len() < n {
        let cycle = find_cycle(&edges, &indegree);
        let names: Vec<&str> = cycle.iter().map(|&i| systems[i].system.name).collect();
        panic!("system ordering cycle: {}", names.join(" -> "));
    }

    // Waves, in sorted order: after every earlier system it is ordered after or conflicts with.
    let mut predecessors: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (from, list) in edges.iter().enumerate() {
        for &to in list {
            predecessors[to].push(from);
        }
    }
    let mut wave = vec![0usize; n];
    for (k, &s) in order.iter().enumerate() {
        let after_ordered = predecessors[s].iter().map(|&p| wave[p] + 1);
        let after_conflicts = order[..k]
            .iter()
            .filter(|&&p| systems[s].system.conflicts_with(&systems[p].system))
            .map(|&p| wave[p] + 1);
        wave[s] = after_ordered.chain(after_conflicts).max().unwrap_or(0);
    }
    let count = wave.iter().max().map_or(0, |w| w + 1);
    let mut waves: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (i, &w) in wave.iter().enumerate() {
        waves[w].push(i);
    }
    Plan { waves, system_sets }
}

/// The ancestors of set `s` (itself first), memoized in `closure`.
fn set_closure(
    s: usize,
    sets: &[SetConfig],
    set_index: &HashMap<&'static str, usize>,
    closure: &mut Vec<Option<Vec<usize>>>,
    path: &mut Vec<usize>,
) {
    if closure[s].is_some() {
        return;
    }
    if let Some(at) = path.iter().position(|&p| p == s) {
        let mut names: Vec<&str> = path[at..].iter().map(|&p| sets[p].name).collect();
        names.push(sets[s].name);
        panic!("system set nesting cycle: {}", names.join(" in "));
    }
    path.push(s);
    let mut all = vec![s];
    for parent in &sets[s].parents {
        let p = set_index[parent];
        set_closure(p, sets, set_index, closure, path);
        all.extend(closure[p].iter().flatten().copied());
    }
    path.pop();
    all.sort_unstable();
    all.dedup();
    closure[s] = Some(all);
}

/// A cycle among the systems Kahn's algorithm could not order (those with edges left).
fn find_cycle(edges: &[Vec<usize>], indegree: &[usize]) -> Vec<usize> {
    let stuck = |i: usize| indegree[i] > 0;
    let start = (0..edges.len())
        .find(|&i| stuck(i))
        .expect("a stuck system");
    // Every stuck system has a stuck predecessor, so walking predecessors must revisit one.
    let mut predecessor = vec![usize::MAX; edges.len()];
    for (from, list) in edges.iter().enumerate() {
        for &to in list {
            if stuck(from) && stuck(to) && predecessor[to] == usize::MAX {
                predecessor[to] = from;
            }
        }
    }
    let mut seen: Vec<usize> = Vec::new();
    let mut at = start;
    while !seen.contains(&at) {
        seen.push(at);
        at = predecessor[at];
    }
    let from = seen.iter().position(|&s| s == at).expect("revisited");
    let mut cycle: Vec<usize> = seen[from..].to_vec();
    cycle.reverse();
    // Start at the earliest-added system, so the report does not depend on where the walk began.
    let first = (0..cycle.len())
        .min_by_key(|&i| cycle[i])
        .expect("non-empty");
    cycle.rotate_left(first);
    cycle.push(cycle[0]);
    cycle
}
