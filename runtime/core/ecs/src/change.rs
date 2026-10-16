//! Change detection: when each component was added and last written, in world ticks.
//!
//! The world keeps a change tick. Every component row records the tick it was added at and the
//! tick it was last written through a [`Mut`]. A reader compares those with two ticks of its own:
//! `last_run`, when it last looked, and `this_run`, now. A row changed after `last_run` is
//! reported once to that reader, whatever other readers have seen.
//!
//! - Systems in a [`Schedule`](crate::Schedule): `last_run` is the system's previous run, so each
//!   system sees what happened since it last ran.
//! - Queries through [`World`](crate::World) directly: `last_run` is the world's
//!   [`last_change_tick`](crate::World::last_change_tick), advanced by
//!   [`clear_trackers`](crate::World::clear_trackers) once a frame.
//!
//! Ticks are `u32`s that wrap. Ticks older than [`MAX_CHANGE_AGE`] are clamped every
//! [`CHECK_TICK_THRESHOLD`] ticks, so a reader that has not looked for that long sees everything
//! as changed rather than missing changes.

use core::cell::UnsafeCell;
use core::fmt;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};

use crate::archetype::Archetype;
use crate::component::{Component, ComponentId};
use crate::query::{Access, QueryFilter};
use crate::storage::ComponentTicks;
use crate::world::World;

/// How often (in ticks) old ticks are clamped.
pub const CHECK_TICK_THRESHOLD: u32 = 518_400_000;
/// The oldest a tick may get before it is clamped: changes older than this read as this old.
pub const MAX_CHANGE_AGE: u32 = u32::MAX - (2 * CHECK_TICK_THRESHOLD - 1);

/// A point in a world's change history.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Tick(pub(crate) u32);

impl Tick {
    pub const fn new(tick: u32) -> Self {
        Self(tick)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    /// Whether `self` happened after `last_run`, as seen at `this_run`.
    #[inline]
    pub fn is_newer_than(self, last_run: Tick, this_run: Tick) -> bool {
        let since_self = this_run.0.wrapping_sub(self.0).min(MAX_CHANGE_AGE);
        let since_last_run = this_run.0.wrapping_sub(last_run.0).min(MAX_CHANGE_AGE);
        since_last_run > since_self
    }

    /// Clamps the tick to at most [`MAX_CHANGE_AGE`] before `now`.
    #[inline]
    pub(crate) fn clamp(&mut self, now: Tick) {
        if now.0.wrapping_sub(self.0) > MAX_CHANGE_AGE {
            self.0 = now.0.wrapping_sub(MAX_CHANGE_AGE);
        }
    }
}

/// A reader's view of time: changes after `last_run`, seen at `this_run`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ChangeTicks {
    pub last_run: Tick,
    pub this_run: Tick,
}

impl ChangeTicks {
    #[inline]
    pub(crate) fn changed(&self, ticks: &ComponentTicks) -> bool {
        ticks.changed.is_newer_than(self.last_run, self.this_run)
    }

    #[inline]
    pub(crate) fn added(&self, ticks: &ComponentTicks) -> bool {
        ticks.added.is_newer_than(self.last_run, self.this_run)
    }
}

/// A row's ticks, shared between readers and the one writer the query rules allow.
#[repr(transparent)]
pub(crate) struct TickCell(UnsafeCell<ComponentTicks>);

impl TickCell {
    pub(crate) fn new(ticks: ComponentTicks) -> Self {
        Self(UnsafeCell::new(ticks))
    }

    #[inline]
    pub(crate) fn get(&self) -> *mut ComponentTicks {
        self.0.get()
    }

    #[inline]
    pub(crate) fn read(&self) -> ComponentTicks {
        // SAFETY: ticks are plain data; writers are excluded by the access rules that also
        // guard the component itself.
        unsafe { *self.0.get() }
    }
}

/// Write access to a component that records the write: [`DerefMut`] marks the row changed.
/// Reading through [`Deref`] does not.
pub struct Mut<'w, T> {
    value: &'w mut T,
    ticks: &'w mut ComponentTicks,
    change: ChangeTicks,
}

impl<'w, T> Mut<'w, T> {
    pub(crate) fn new(
        value: &'w mut T,
        ticks: &'w mut ComponentTicks,
        change: ChangeTicks,
    ) -> Self {
        Self {
            value,
            ticks,
            change,
        }
    }

    /// Changed since the reader last looked (added counts as changed).
    pub fn is_changed(&self) -> bool {
        self.change.changed(self.ticks)
    }

    pub fn is_added(&self) -> bool {
        self.change.added(self.ticks)
    }

    /// The tick of the last recorded write.
    pub fn last_changed(&self) -> Tick {
        self.ticks.changed
    }

    /// Marks the row changed without writing.
    pub fn set_changed(&mut self) {
        self.ticks.changed = self.change.this_run;
    }

    /// The value without marking it changed, for writes no reader should hear about.
    pub fn bypass_change_detection(&mut self) -> &mut T {
        self.value
    }

    /// Writes `value` only if it differs, so equal writes are not reported as changes.
    pub fn set_if_neq(&mut self, value: T)
    where
        T: PartialEq,
    {
        if *self.value != value {
            *self.value = value;
            self.set_changed();
        }
    }

    /// The plain reference, marked changed.
    pub fn into_inner(self) -> &'w mut T {
        self.ticks.changed = self.change.this_run;
        self.value
    }
}

impl<T> Deref for Mut<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        self.value
    }
}

impl<T> DerefMut for Mut<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        self.set_changed();
        self.value
    }
}

impl<T: fmt::Debug> fmt::Debug for Mut<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.value.fmt(f)
    }
}

/// Read access to a component with its change state.
pub struct Ref<'w, T> {
    value: &'w T,
    ticks: ComponentTicks,
    change: ChangeTicks,
}

impl<'w, T> Ref<'w, T> {
    pub(crate) fn new(value: &'w T, ticks: ComponentTicks, change: ChangeTicks) -> Self {
        Self {
            value,
            ticks,
            change,
        }
    }

    pub fn is_changed(&self) -> bool {
        self.change.changed(&self.ticks)
    }

    pub fn is_added(&self) -> bool {
        self.change.added(&self.ticks)
    }

    pub fn last_changed(&self) -> Tick {
        self.ticks.changed
    }

    pub fn into_inner(self) -> &'w T {
        self.value
    }
}

impl<T> Deref for Ref<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        self.value
    }
}

impl<T: fmt::Debug> fmt::Debug for Ref<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.value.fmt(f)
    }
}

// --- Filters ---

/// Entities whose `T` was written (or added) since the reader last looked.
pub struct Changed<T>(PhantomData<fn() -> T>);

/// Entities whose `T` was added since the reader last looked.
pub struct Added<T>(PhantomData<fn() -> T>);

/// The ticks column of `T` in an archetype, for the row filters.
#[derive(Clone, Copy)]
pub struct TickFetch {
    ticks: *const TickCell,
    change: ChangeTicks,
}

fn tick_fetch(id: &Option<ComponentId>, archetype: &Archetype, change: ChangeTicks) -> TickFetch {
    // `matches` admitted only archetypes with the column.
    let column = id
        .and_then(|id| archetype.column(id))
        .expect("filtered archetype has the column");
    TickFetch {
        ticks: column.ticks_ptr(),
        change,
    }
}

macro_rules! tick_filter {
    ($Filter:ident, $test:ident) => {
        impl<T: Component> QueryFilter for $Filter<T> {
            type State = Option<ComponentId>;
            type Fetch = TickFetch;
            const ROW_FILTER: bool = true;

            fn access(access: &mut Access) {
                access.read::<T>();
            }

            fn state(world: &World) -> Option<ComponentId> {
                world.component_id::<T>()
            }

            fn matches(id: &Option<ComponentId>, archetype: &Archetype) -> bool {
                id.is_some_and(|id| archetype.contains(id))
            }

            unsafe fn fetch(
                id: &Option<ComponentId>,
                archetype: &Archetype,
                change: ChangeTicks,
            ) -> TickFetch {
                tick_fetch(id, archetype, change)
            }

            unsafe fn filter(fetch: TickFetch, row: usize) -> bool {
                // SAFETY: `row` is in bounds (the caller); the access declares a read of `T`.
                let ticks = unsafe { (*fetch.ticks.add(row)).read() };
                fetch.change.$test(&ticks)
            }
        }
    };
}

tick_filter!(Changed, changed);
tick_filter!(Added, added);
