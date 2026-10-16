//! Systems and their automatically parallel schedule.
//!
//! A system declares the component and resource types it reads and writes. The schedule places
//! each system in the earliest *wave* after every earlier system it conflicts with, then runs each
//! wave's systems in parallel on the job system. Systems that conflict therefore always run in the
//! order they were added; the others run in whatever order the workers pick them up. Every access
//! a system makes is checked against its declaration at runtime, which is what makes running
//! systems in parallel through a shared world sound.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::mem;

use marlin_jobs::JobSystem;

use crate::change::{ChangeTicks, Mut, Tick};
use crate::command::Commands;
use crate::component::Component;
use crate::entity::Entity;
use crate::query::{Access, QueryData, QueryFilter, QueryIter, ReadOnlyQueryData, query_access};
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

    #[track_caller]
    pub fn get<T: Component>(&self, entity: Entity) -> Option<&T> {
        self.check(Access::new().read::<T>());
        // SAFETY: declared read.
        self.world
            .component_ptr::<T>(entity)
            .map(|p| unsafe { &*p })
    }

    /// The entity's `T` for writing; writes through the [`Mut`] mark it changed.
    #[track_caller]
    pub fn get_mut<T: Component>(&mut self, entity: Entity) -> Option<Mut<'_, T>> {
        self.check(Access::new().write::<T>());
        let (value, ticks) = self.world.component_and_ticks_ptr::<T>(entity)?;
        // SAFETY: declared write; `&mut self` keeps it unique within the system, and no other
        // system touches `T` meanwhile.
        Some(unsafe { Mut::new(&mut *value, &mut *ticks, self.change) })
    }

    pub fn is_alive(&self, entity: Entity) -> bool {
        self.world.is_alive(entity)
    }

    /// For nested parallelism inside the system.
    pub fn jobs(&self) -> &JobSystem {
        self.jobs
    }
}

/// What a system receives each run.
pub struct SystemContext<'w> {
    pub world: SystemWorld<'w>,
    /// Applied after the system's wave, in system order.
    pub commands: Commands,
}

type SystemFn = Box<dyn FnMut(&mut SystemContext<'_>) + Send>;
type ExclusiveFn = Box<dyn FnMut(&mut World, &JobSystem) + Send>;

enum Run {
    Shared(SystemFn),
    /// Runs alone with the whole world.
    Exclusive(ExclusiveFn),
}

pub struct System {
    name: &'static str,
    access: Access,
    run: Run,
}

impl System {
    /// A system that runs in parallel with others it does not conflict with. Declare what it
    /// touches with [`reads`](Self::reads) / [`writes`](Self::writes).
    pub fn new(
        name: &'static str,
        run: impl FnMut(&mut SystemContext<'_>) + Send + 'static,
    ) -> Self {
        Self {
            name,
            access: Access::new(),
            run: Run::Shared(Box::new(run)),
        }
    }

    /// A system with `&mut World`, running alone between the systems added before and after it.
    pub fn exclusive(
        name: &'static str,
        run: impl FnMut(&mut World, &JobSystem) + Send + 'static,
    ) -> Self {
        Self {
            name,
            access: Access::new(),
            run: Run::Exclusive(Box::new(run)),
        }
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

    pub fn name(&self) -> &'static str {
        self.name
    }

    fn conflicts_with(&self, other: &Self) -> bool {
        matches!(self.run, Run::Exclusive(_))
            || matches!(other.run, Run::Exclusive(_))
            || self.access.conflicts_with(&other.access)
    }
}

struct Entry {
    system: System,
    commands: Commands,
    /// The tick of the system's previous run (0 before the first: everything reads as added).
    last_run: Tick,
}

#[derive(Default)]
pub struct Schedule {
    systems: Vec<Entry>,
    /// Wave of each system; rebuilt after systems change.
    waves: Option<Vec<usize>>,
}

impl Schedule {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_system(&mut self, system: System) -> &mut Self {
        self.systems.push(Entry {
            system,
            commands: Commands::new(),
            last_run: Tick::default(),
        });
        self.waves = None;
        self
    }

    /// Each system's wave: one past the latest earlier system it conflicts with.
    fn waves(&mut self) -> &[usize] {
        self.waves.get_or_insert_with(|| {
            let mut waves: Vec<usize> = Vec::with_capacity(self.systems.len());
            for (i, entry) in self.systems.iter().enumerate() {
                let wave = (0..i)
                    .filter(|&j| entry.system.conflicts_with(&self.systems[j].system))
                    .map(|j| waves[j] + 1)
                    .max()
                    .unwrap_or(0);
                waves.push(wave);
            }
            waves
        })
    }

    /// Names of the systems in each wave, for inspection.
    pub fn wave_names(&mut self) -> Vec<Vec<&'static str>> {
        let waves = self.waves().to_vec();
        let count = waves.iter().max().map_or(0, |w| w + 1);
        (0..count)
            .map(|w| {
                self.systems
                    .iter()
                    .zip(&waves)
                    .filter(|&(_, &x)| x == w)
                    .map(|(e, _)| e.system.name)
                    .collect()
            })
            .collect()
    }

    /// Runs every system once, wave by wave, then applies their commands.
    pub fn run(&mut self, world: &mut World, jobs: &JobSystem) {
        let waves = self.waves().to_vec();
        let count = waves.iter().max().map_or(0, |w| w + 1);
        for wave in 0..count {
            let mut members = self
                .systems
                .iter_mut()
                .zip(&waves)
                .filter(|&(_, &w)| w == wave)
                .map(|(e, _)| e);
            let first = members.next().expect("waves are contiguous");
            let rest: Vec<&mut Entry> = members.collect();

            if let Run::Exclusive(run) = &mut first.system.run {
                debug_assert!(rest.is_empty(), "exclusive systems run alone");
                first.last_run = world.increment_change_tick();
                run(world, jobs);
                continue;
            }

            let shared: &World = world;
            if rest.is_empty() {
                Self::run_shared(first, shared, jobs);
            } else {
                jobs.scope(|s| {
                    s.spawn(|| Self::run_shared(first, shared, jobs));
                    for entry in rest {
                        s.spawn(|| Self::run_shared(entry, shared, jobs));
                    }
                });
            }

            // Commands in system order, whatever order the systems finished in.
            for entry in self
                .systems
                .iter_mut()
                .zip(&waves)
                .filter(|&(_, &w)| w == wave)
                .map(|(e, _)| e)
            {
                world.apply(&mut entry.commands);
            }
        }
        if let Some(now) = world.check_change_ticks() {
            for entry in &mut self.systems {
                entry.last_run.clamp(now);
            }
        }
    }

    fn run_shared(entry: &mut Entry, world: &World, jobs: &JobSystem) {
        let Run::Shared(run) = &mut entry.system.run else {
            unreachable!("exclusive systems run separately")
        };
        let this_run = world.increment_change_tick();
        let mut context = SystemContext {
            world: SystemWorld {
                world,
                access: &entry.system.access,
                name: entry.system.name,
                jobs,
                change: ChangeTicks {
                    last_run: entry.last_run,
                    this_run,
                },
            },
            commands: mem::take(&mut entry.commands),
        };
        run(&mut context);
        entry.commands = context.commands;
        entry.last_run = this_run;
    }
}
