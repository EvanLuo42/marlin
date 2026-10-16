//! Deferred structural changes, recorded while the world is shared and applied afterwards.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::bundle::Bundle;
use crate::component::Component;
use crate::entity::Entity;
use crate::world::World;

type Command = Box<dyn FnOnce(&mut World) + Send>;

/// A queue of world changes. Systems cannot spawn, despawn, add or remove components while other
/// systems run, so they record those here; the schedule applies each system's queue in system
/// order once its wave finishes, which keeps results deterministic.
#[derive(Default)]
pub struct Commands {
    queue: Vec<Command>,
}

impl Commands {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spawn<B: Bundle>(&mut self, bundle: B) {
        self.add(move |world| {
            world.spawn(bundle);
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

    pub fn remove<T: Component>(&mut self, entity: Entity) {
        self.add(move |world| {
            world.remove::<T>(entity);
        });
    }

    pub fn insert_resource<R: Send + Sync + 'static>(&mut self, resource: R) {
        self.add(move |world| world.insert_resource(resource));
    }

    /// Any change to the world.
    pub fn add(&mut self, command: impl FnOnce(&mut World) + Send + 'static) {
        self.queue.push(Box::new(command));
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

impl World {
    /// Applies the queued commands in order, leaving the queue empty.
    pub fn apply(&mut self, commands: &mut Commands) {
        for command in commands.queue.drain(..) {
            command(self);
        }
    }
}
