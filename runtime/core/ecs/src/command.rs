//! Deferred structural changes, recorded while the world is shared and applied afterwards.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::bundle::Bundle;
use crate::component::Component;
use crate::entity::{Entities, Entity};
use crate::event::Event;
use crate::world::{World, WorldId};

type Command = Box<dyn FnOnce(&mut World) + Send>;

/// Recorded commands, applied in order by [`World::apply`]. Each system owns one; a queue that
/// recorded entity reservations belongs to that world.
#[derive(Default)]
pub struct CommandQueue {
    queue: Vec<Command>,
    /// The world whose ids [`Commands`] reserved for this queue.
    world: Option<WorldId>,
}

impl CommandQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, command: impl FnOnce(&mut World) + Send + 'static) {
        self.queue.push(Box::new(command));
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Moves `other`'s commands to the end of this queue.
    pub fn append(&mut self, other: &mut Self) {
        if other.world.is_some() {
            self.world = other.world;
        }
        self.queue.append(&mut other.queue);
    }
}

/// Records world changes. Systems cannot spawn, despawn, add or remove components while other
/// systems run, so they record those here; the schedule applies each system's queue in system
/// order once its wave finishes, which keeps results deterministic.
///
/// [`spawn`](Self::spawn) reserves the entity id right away, so later commands in the same queue
/// (or anything else) can refer to it. A reserved id becomes a live entity without components
/// when the world next changes structurally (at the latest when the queue is applied); the spawn
/// command then gives it its bundle.
pub struct Commands<'w, 's> {
    queue: &'s mut CommandQueue,
    entities: &'w Entities,
}

impl<'w, 's> Commands<'w, 's> {
    /// Commands recording into `queue`, reserving entity ids from `world`.
    pub fn new(queue: &'s mut CommandQueue, world: &'w World) -> Self {
        Self::from_entities(queue, &world.entities, world.id())
    }

    pub(crate) fn from_entities(
        queue: &'s mut CommandQueue,
        entities: &'w Entities,
        world: WorldId,
    ) -> Self {
        debug_assert!(queue.world.is_none_or(|w| w == world));
        queue.world = Some(world);
        Self { queue, entities }
    }

    /// Reserves an entity id and records spawning it with `bundle`.
    pub fn spawn<B: Bundle>(&mut self, bundle: B) -> Entity {
        let entity = self.entities.reserve();
        self.add(move |world| {
            world.insert_bundle(entity, bundle);
        });
        entity
    }

    /// Reserves an entity id; the entity has no components.
    pub fn spawn_empty(&mut self) -> Entity {
        self.entities.reserve()
    }

    /// Records [`World::spawn_batch`].
    pub fn spawn_batch<I>(&mut self, bundles: I)
    where
        I: IntoIterator + Send + 'static,
        I::Item: Bundle,
    {
        self.add(move |world| {
            world.spawn_batch(bundles);
        });
    }

    pub fn despawn(&mut self, entity: Entity) {
        self.add(move |world| {
            world.despawn(entity);
        });
    }

    pub fn insert<T: Component>(&mut self, entity: Entity, value: T) {
        self.add(move |world| {
            world.insert(entity, value);
        });
    }

    pub fn insert_bundle<B: Bundle>(&mut self, entity: Entity, bundle: B) {
        self.add(move |world| {
            world.insert_bundle(entity, bundle);
        });
    }

    pub fn remove<T: Component>(&mut self, entity: Entity) {
        self.add(move |world| {
            world.remove::<T>(entity);
        });
    }

    pub fn insert_resource<R: Send + Sync + 'static>(&mut self, resource: R) {
        self.add(move |world| world.insert_resource(resource));
    }

    pub fn remove_resource<R: Send + Sync + 'static>(&mut self) {
        self.add(move |world| {
            world.remove_resource::<R>();
        });
    }

    /// Records [`World::send_event`].
    pub fn send_event<E: Event>(&mut self, event: E) {
        self.add(move |world| world.send_event(event));
    }

    /// Any change to the world.
    pub fn add(&mut self, command: impl FnOnce(&mut World) + Send + 'static) {
        self.queue.push(command);
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// The entity table ids are reserved from, for helpers in this crate.
    pub(crate) fn entities(&self) -> &'w Entities {
        self.entities
    }
}

impl World {
    /// Applies the queued commands in order, leaving the queue empty. Ids reserved through the
    /// queue's [`Commands`] become live entities first.
    ///
    /// # Panics
    /// If the queue reserved ids from another world.
    pub fn apply(&mut self, commands: &mut CommandQueue) {
        assert!(
            commands.world.is_none_or(|w| w == self.id()),
            "a command queue was applied to a world other than the one it reserved ids from"
        );
        commands.world = None;
        self.flush();
        for command in commands.queue.drain(..) {
            command(self);
        }
        self.flush();
    }
}
