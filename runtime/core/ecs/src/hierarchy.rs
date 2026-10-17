//! Parent/child relationships between entities.
//!
//! A child has a [`Parent`]; a parent has [`Children`], in the order they were attached. Both
//! sides are only created by the operations here ([`World::set_parent`],
//! [`World::remove_parent`], [`World::despawn_recursive`] and their [`Commands`] versions), which
//! update both at once. Component hooks keep them consistent when one side goes away by other
//! means: despawning or removing a child's `Parent` detaches it from its parent, despawning a
//! parent (or removing its `Children`) orphans its children, and cloning a child attaches the
//! clone to the same parent. Those hook fixes land when the triggering operation returns.
//!
//! Transform propagation is not here: it belongs to the world layer above.

use alloc::vec;
use alloc::vec::Vec;
use core::mem;
use core::ops::Deref;

use crate::bundle::Bundle;
use crate::command::Commands;
use crate::component::HookContext;
use crate::entity::Entity;
use crate::world::World;

/// The entity's parent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Parent(pub(crate) Entity);

impl Parent {
    pub fn get(&self) -> Entity {
        self.0
    }
}

/// The entity's children, in the order they were attached.
#[derive(PartialEq, Eq, Debug, Default)]
pub struct Children(pub(crate) Vec<Entity>);

impl Children {
    pub fn as_slice(&self) -> &[Entity] {
        &self.0
    }
}

impl Deref for Children {
    type Target = [Entity];

    fn deref(&self) -> &[Entity] {
        &self.0
    }
}

/// Registers the hierarchy components and their hooks.
pub(crate) fn register(world: &mut World) {
    world.register_cloneable::<Parent>();
    world
        .component_hooks::<Parent>()
        .on_insert(parent_inserted)
        .on_replace(parent_replaced);
    world.register_component::<Children>();
    world
        .component_hooks::<Children>()
        .on_remove(children_removed);
}

fn parent_of(world: &World, child: Entity) -> Option<Entity> {
    world.get::<Parent>(child).map(|p| p.0)
}

fn lists(world: &World, parent: Entity, child: Entity) -> bool {
    world
        .get::<Children>(parent)
        .is_some_and(|c| c.0.contains(&child))
}

/// A `Parent` arrived (a clone, or a plain insert): make the parent list the child.
fn parent_inserted(ctx: &mut HookContext<'_, '_>) {
    let child = ctx.entity;
    let Some(parent) = parent_of(ctx.world, child) else {
        return;
    };
    if !lists(ctx.world, parent, child) {
        ctx.commands.add(move |world| {
            if parent_of(world, child) == Some(parent) {
                world.link_child(parent, child);
            }
        });
    }
}

/// A `Parent` is about to be overwritten or removed: detach the child from that parent, unless it
/// is attached to it again by the time the fix runs.
fn parent_replaced(ctx: &mut HookContext<'_, '_>) {
    let child = ctx.entity;
    let Some(parent) = parent_of(ctx.world, child) else {
        return;
    };
    if lists(ctx.world, parent, child) {
        ctx.commands.add(move |world| {
            if parent_of(world, child) != Some(parent) {
                world.unlink_child(parent, child);
            }
        });
    }
}

/// A parent's `Children` is going away (usually with the parent): orphan the children it lists.
fn children_removed(ctx: &mut HookContext<'_, '_>) {
    let parent = ctx.entity;
    let Some(children) = ctx.world.get::<Children>(parent) else {
        return;
    };
    if children.0.is_empty() {
        return;
    }
    let children = children.0.clone();
    ctx.commands.add(move |world| {
        for child in children {
            if parent_of(world, child) == Some(parent) && !lists(world, parent, child) {
                world.remove::<Parent>(child);
            }
        }
    });
}

impl World {
    /// Makes `parent` the parent of `child`, detaching it from any previous parent; it goes last
    /// among `parent`'s children. Returns false if either is not alive.
    ///
    /// # Panics
    /// If `child` is `parent` or one of its ancestors: that would make a cycle.
    #[track_caller]
    pub fn set_parent(&mut self, child: Entity, parent: Entity) -> bool {
        assert!(child != parent, "{child:?} cannot be its own parent");
        if !self.is_alive(child) || !self.is_alive(parent) {
            return false;
        }
        assert!(
            !self.ancestors(parent).any(|a| a == child),
            "making {parent:?} the parent of {child:?} would make a cycle: it descends from it"
        );
        let old = parent_of(self, child);
        if old == Some(parent) {
            return true;
        }
        self.defer_hooks(|world| {
            if let Some(old) = old {
                world.unlink_child(old, child);
            }
            world.insert(child, Parent(parent));
            world.link_child(parent, child);
        });
        true
    }

    /// Detaches `child` from its parent; false if it had none (or is not alive).
    pub fn remove_parent(&mut self, child: Entity) -> bool {
        let Some(parent) = parent_of(self, child) else {
            return false;
        };
        self.defer_hooks(|world| {
            world.unlink_child(parent, child);
            world.remove::<Parent>(child);
        });
        true
    }

    /// Despawns `entity` and all its descendants, detaching it from its parent. Returns false if
    /// it was not alive.
    pub fn despawn_recursive(&mut self, entity: Entity) -> bool {
        if !self.is_alive(entity) {
            return false;
        }
        let mut all = vec![entity];
        all.extend(self.descendants(entity));
        let parent = parent_of(self, entity);
        // Hook fixes would only tidy entities about to go; they run after, as no-ops.
        self.defer_hooks(|world| {
            if let Some(parent) = parent {
                world.unlink_child(parent, entity);
            }
            for e in all {
                world.despawn(e);
            }
        });
        true
    }

    /// The entity's parent.
    pub fn parent(&self, entity: Entity) -> Option<Entity> {
        parent_of(self, entity)
    }

    /// The entity's children, in attachment order; empty if it has none.
    pub fn children(&self, entity: Entity) -> &[Entity] {
        self.get::<Children>(entity).map_or(&[], |c| &c.0)
    }

    /// The entity's descendants, depth first, each parent before its children.
    pub fn descendants(&self, entity: Entity) -> impl Iterator<Item = Entity> + '_ {
        descendants(entity, |e| self.children(e))
    }

    /// The entity's parent, its parent, and so on up to the root.
    pub fn ancestors(&self, entity: Entity) -> impl Iterator<Item = Entity> + '_ {
        core::iter::successors(parent_of(self, entity), |&e| parent_of(self, e))
    }

    /// The topmost ancestor, or the entity itself if it has no parent.
    pub fn root(&self, entity: Entity) -> Entity {
        self.ancestors(entity).last().unwrap_or(entity)
    }

    /// Adds `child` to the end of `parent`'s children if it is not listed.
    pub(crate) fn link_child(&mut self, parent: Entity, child: Entity) {
        if !self.is_alive(parent) {
            return;
        }
        if let Some(mut children) = self.get_mut::<Children>(parent) {
            if !children.0.contains(&child) {
                children.0.push(child);
            }
        } else {
            self.insert(parent, Children(vec![child]));
        }
    }

    /// Removes `child` from `parent`'s children, and the `Children` once empty.
    pub(crate) fn unlink_child(&mut self, parent: Entity, child: Entity) {
        let Some(mut children) = self.get_mut::<Children>(parent) else {
            return;
        };
        let Some(at) = children.0.iter().position(|&c| c == child) else {
            return;
        };
        children.0.remove(at);
        if children.0.is_empty() {
            self.remove::<Children>(parent);
        }
    }

    /// Runs `f` with hook commands held back until it returns, then applies them.
    pub(crate) fn defer_hooks(&mut self, f: impl FnOnce(&mut Self)) {
        let outer = mem::replace(&mut self.applying_hooks, true);
        f(self);
        self.applying_hooks = outer;
        self.drain_hooks();
    }
}

/// The descendants of `root`, depth first, each parent before its children, as `children`
/// reports them. Works on any view of the hierarchy, e.g. a `Query<&Children>`:
/// `descendants(root, |e| query.get(e).map_or(&[][..], |c| c.as_slice()))`.
pub fn descendants<'a>(
    root: Entity,
    children: impl Fn(Entity) -> &'a [Entity] + 'a,
) -> impl Iterator<Item = Entity> + 'a {
    let mut stack: Vec<Entity> = children(root).iter().rev().copied().collect();
    core::iter::from_fn(move || {
        let next = stack.pop()?;
        stack.extend(children(next).iter().rev());
        Some(next)
    })
}

impl Commands<'_, '_> {
    /// Records [`World::set_parent`].
    pub fn set_parent(&mut self, child: Entity, parent: Entity) {
        self.add(move |world| {
            world.set_parent(child, parent);
        });
    }

    /// Records [`World::remove_parent`].
    pub fn remove_parent(&mut self, child: Entity) {
        self.add(move |world| {
            world.remove_parent(child);
        });
    }

    /// Records [`World::despawn_recursive`].
    pub fn despawn_recursive(&mut self, entity: Entity) {
        self.add(move |world| {
            world.despawn_recursive(entity);
        });
    }

    /// Spawns an entity with `bundle` as the last child of `parent`.
    pub fn spawn_child<B: Bundle>(&mut self, parent: Entity, bundle: B) -> Entity {
        let child = self.entities().reserve();
        self.add(move |world| {
            world.insert_bundle(child, bundle);
            world.set_parent(child, parent);
        });
        child
    }
}
