//! Events: messages between systems, double buffered so each lives for two updates.
//!
//! An [`Events<E>`] resource holds the events of the current and the previous update.
//! [`EventWriter`] appends; each [`EventReader`] keeps its own cursor, so every reader sees every
//! event once, whatever order the systems run in, as long as it runs at least once per update.
//! Something must call [`Events::update`] once a frame, normally the [`update_events`] system:
//!
//! ```
//! use marlin_ecs::{EventReader, EventWriter, IntoSystem, Schedule, World, update_events};
//! # use marlin_jobs::{Config, JobSystem};
//!
//! struct Hit(u32);
//!
//! fn shoot(mut hits: EventWriter<Hit>) {
//!     hits.send(Hit(5));
//! }
//!
//! fn score(mut hits: EventReader<Hit>, mut total: marlin_ecs::ResMut<u32>) {
//!     *total += hits.read().map(|h| h.0).sum::<u32>();
//! }
//!
//! let mut world = World::new();
//! world.add_event::<Hit>();
//! world.insert_resource(0u32);
//! let mut schedule = Schedule::new();
//! schedule
//!     .add_system(update_events::<Hit>.before("shoot"))
//!     .add_system(shoot)
//!     .add_system(score.after("shoot"));
//! # let jobs = JobSystem::new(Config { workers: 1, ..Config::default() }).unwrap();
//! schedule.run(&mut world, &jobs);
//! schedule.run(&mut world, &jobs);
//! assert_eq!(*world.resource::<u32>().unwrap(), 10);
//! ```

use alloc::vec::Vec;
use core::mem;

use crate::system::ResMut;
use crate::world::World;

/// Any `Send + Sync + 'static` type can be an event.
pub trait Event: Send + Sync + 'static {}

impl<T: Send + Sync + 'static> Event for T {}

/// The events of type `E` sent in this update and the previous one.
pub struct Events<E> {
    /// The previous update's events; the first one has id `old_start`.
    old: Vec<E>,
    old_start: usize,
    /// This update's events; the first one has id `new_start`.
    new: Vec<E>,
    new_start: usize,
}

impl<E> Default for Events<E> {
    fn default() -> Self {
        Self {
            old: Vec::new(),
            old_start: 0,
            new: Vec::new(),
            new_start: 0,
        }
    }
}

impl<E: Event> Events<E> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn send(&mut self, event: E) {
        self.new.push(event);
    }

    pub fn send_batch(&mut self, events: impl IntoIterator<Item = E>) {
        self.new.extend(events);
    }

    /// Ends an update: this update's events become the previous ones, and the previous ones are
    /// dropped.
    pub fn update(&mut self) {
        mem::swap(&mut self.old, &mut self.new);
        self.new.clear();
        self.old_start = self.new_start;
        self.new_start = self.old_start + self.old.len();
    }

    /// Drops every event.
    pub fn clear(&mut self) {
        self.update();
        self.update();
    }

    /// Events ever sent.
    pub fn count(&self) -> usize {
        self.new_start + self.new.len()
    }

    /// Events still buffered.
    pub fn len(&self) -> usize {
        self.old.len() + self.new.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Buffered events with ids from `cursor` on, oldest first.
    pub fn read_from(&self, cursor: usize) -> impl Iterator<Item = &E> + '_ {
        let old = cursor.saturating_sub(self.old_start).min(self.old.len());
        let new = cursor.saturating_sub(self.new_start).min(self.new.len());
        self.old[old..].iter().chain(&self.new[new..])
    }

    /// Every buffered event, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &E> + '_ {
        self.read_from(0)
    }

    /// Takes every buffered event out, oldest first.
    pub fn drain(&mut self) -> impl Iterator<Item = E> + '_ {
        self.old_start = self.count();
        self.new_start = self.old_start;
        self.old.drain(..).chain(self.new.drain(..))
    }
}

/// A system parameter that appends `E`s; it writes the [`Events<E>`] resource.
pub struct EventWriter<'w, E: Event> {
    pub(crate) events: &'w mut Events<E>,
}

impl<E: Event> EventWriter<'_, E> {
    pub fn send(&mut self, event: E) {
        self.events.send(event);
    }

    pub fn send_batch(&mut self, events: impl IntoIterator<Item = E>) {
        self.events.send_batch(events);
    }
}

/// A system parameter that reads the `E`s it has not seen yet; it reads the [`Events<E>`]
/// resource. Its cursor lives with the system, so each reader sees each event once.
pub struct EventReader<'w, 's, E: Event> {
    pub(crate) events: &'w Events<E>,
    pub(crate) cursor: &'s mut usize,
}

impl<E: Event> EventReader<'_, '_, E> {
    /// The unseen events, oldest first; they count as seen afterwards.
    pub fn read(&mut self) -> impl Iterator<Item = &E> + '_ {
        let from = *self.cursor;
        *self.cursor = self.events.count();
        self.events.read_from(from)
    }

    /// Unseen events.
    pub fn len(&self) -> usize {
        self.events.read_from(*self.cursor).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Marks every event seen.
    pub fn clear(&mut self) {
        *self.cursor = self.events.count();
    }
}

/// Advances `Events<E>` once per run: add it to the schedule once per event type.
pub fn update_events<E: Event>(mut events: ResMut<'_, Events<E>>) {
    events.update();
}

impl World {
    /// Adds the [`Events<E>`] resource if it is missing.
    pub fn add_event<E: Event>(&mut self) {
        if !self.has_resource::<Events<E>>() {
            self.insert_resource(Events::<E>::new());
        }
    }

    /// Sends an event.
    ///
    /// # Panics
    /// If `E` was not added with [`add_event`](Self::add_event).
    #[track_caller]
    pub fn send_event<E: Event>(&mut self, event: E) {
        self.resource_mut::<Events<E>>()
            .unwrap_or_else(|| panic!("{}", missing_events::<E>()))
            .send(event);
    }
}

pub(crate) fn missing_events<E>() -> alloc::string::String {
    alloc::format!(
        "events of type `{}` were never added (World::add_event)",
        core::any::type_name::<E>()
    )
}
