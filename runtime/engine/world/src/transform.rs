//! Positions in the world: [`Transform`] (relative to the parent), [`GlobalTransform`] (in the
//! world) and the propagation from one to the other (0019 §6).

use alloc::vec::Vec;

use marlin_containers::{HashSet, HashSetExt};
use marlin_ecs::{Added, Changed, Children, Entity, Parent, With, Without};
use marlin_math::{DVec3, Quat, Vec3};
use marlin_reflect::Reflect;

/// Where an entity is relative to its parent, or to the world without one: scale, then rotate,
/// then translate. The translation is `f64` so positions far from the origin stay precise.
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(name = "marlin_world::Transform", default)]
pub struct Transform {
    #[reflect(units = "m")]
    pub translation: DVec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    pub const IDENTITY: Self = Self {
        translation: DVec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    pub const fn from_translation(translation: DVec3) -> Self {
        Self {
            translation,
            ..Self::IDENTITY
        }
    }

    pub const fn from_rotation(rotation: Quat) -> Self {
        Self {
            rotation,
            ..Self::IDENTITY
        }
    }

    pub const fn from_scale(scale: Vec3) -> Self {
        Self {
            scale,
            ..Self::IDENTITY
        }
    }

    pub const fn with_rotation(self, rotation: Quat) -> Self {
        Self { rotation, ..self }
    }

    pub const fn with_scale(self, scale: Vec3) -> Self {
        Self { scale, ..self }
    }

    /// An `f32` transform's values (exact).
    pub fn from_f32(t: &marlin_math::Transform) -> Self {
        Self {
            translation: DVec3::from_vec3(t.translation),
            rotation: t.rotation,
            scale: t.scale,
        }
    }

    /// `child` (relative to this transform) relative to this one's parent: placing a sublevel's
    /// roots, composing a Prefab's.
    pub fn mul_transform(&self, child: &Transform) -> Self {
        Self {
            translation: self.translation
                + child.translation.scale(self.scale).rotate(self.rotation),
            rotation: (self.rotation * child.rotation).normalize(),
            scale: self.scale * child.scale,
        }
    }

    pub fn forward(&self) -> Vec3 {
        self.rotation * Vec3::FORWARD
    }

    pub fn right(&self) -> Vec3 {
        self.rotation * Vec3::RIGHT
    }

    pub fn up(&self) -> Vec3 {
        self.rotation * Vec3::UP
    }
}

/// Where an entity is in the world, computed from its [`Transform`] and its ancestors' in the
/// [`TickGroup::TransformPropagate`](crate::TickGroup::TransformPropagate) group. Not saved:
/// written only by propagation (and by systems that own an entity's motion, such as physics,
/// which then also write its `Transform`).
///
/// Composition is exact for uniform scale; with non-uniform parent scale and rotation it
/// approximates the shear TRS cannot represent, as `marlin_math::Transform` does.
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(name = "marlin_world::GlobalTransform", default)]
pub struct GlobalTransform {
    pub translation: DVec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for GlobalTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl From<Transform> for GlobalTransform {
    fn from(t: Transform) -> Self {
        Self {
            translation: t.translation,
            rotation: t.rotation,
            scale: t.scale,
        }
    }
}

impl GlobalTransform {
    pub const IDENTITY: Self = Self {
        translation: DVec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    /// `child` (relative to this one) in the world.
    pub fn mul_transform(&self, child: &Transform) -> Self {
        Self {
            translation: self.transform_point(child.translation),
            rotation: (self.rotation * child.rotation).normalize(),
            scale: self.scale * child.scale,
        }
    }

    /// A point given relative to this transform, in the world.
    pub fn transform_point(&self, p: DVec3) -> DVec3 {
        self.translation + p.scale(self.scale).rotate(self.rotation)
    }

    pub fn transform_vector(&self, v: Vec3) -> Vec3 {
        self.rotation * (v * self.scale)
    }

    /// This transform in `f32`, relative to `origin`: what code working near `origin` (a view, a
    /// physics region) uses.
    pub fn relative_to(&self, origin: DVec3) -> marlin_math::Transform {
        marlin_math::Transform {
            translation: self.translation.relative_to(origin),
            rotation: self.rotation,
            scale: self.scale,
        }
    }

    /// Between `self` (at `t = 0`) and `next`: translation and scale linearly, rotation along the
    /// shortest arc. For drawing fixed-step motion between steps.
    pub fn lerp(&self, next: &Self, t: f32) -> Self {
        Self {
            translation: self.translation.lerp(next.translation, t as f64),
            rotation: self.rotation.slerp(next.rotation, t),
            scale: self.scale + (next.scale - self.scale) * t,
        }
    }

    pub fn forward(&self) -> Vec3 {
        self.rotation * Vec3::FORWARD
    }
}

/// Computes [`GlobalTransform`]s: the transform propagation system, alone in
/// [`TickGroup::TransformPropagate`](crate::TickGroup::TransformPropagate).
///
/// Entities with a [`Transform`] get a `GlobalTransform` if they lack one. A global transform is
/// rewritten (so reads as changed) only when it moves: when the entity's `Transform` changed, its
/// [`Parent`] was set, changed or removed, it just got its `GlobalTransform`, or an ancestor's
/// global transform was rewritten. "Changed" is the world's window: since the last
/// `clear_trackers`, which the world runs at the end of each frame. An entity without a
/// `Transform` breaks the chain: its children are placed relative to the world.
///
/// Every entity with a transform is visited, but only moved ones are written, so extract systems
/// filtering on `Changed<GlobalTransform>` see exactly what moved.
pub fn propagate_transforms(world: &mut marlin_ecs::World) {
    let missing: Vec<Entity> = world
        .query_ref_filtered::<Entity, (With<Transform>, Without<GlobalTransform>)>()
        .collect();
    for e in missing {
        world.insert(e, GlobalTransform::IDENTITY);
    }

    let mut moved: HashSet<Entity> = HashSet::new();
    moved.extend(world.query_ref_filtered::<Entity, Changed<Transform>>());
    moved.extend(world.query_ref_filtered::<Entity, (Changed<Parent>, With<Transform>)>());
    moved.extend(world.query_ref_filtered::<Entity, Added<GlobalTransform>>());
    // Entities that lost their parent this frame (removed, or the parent despawned).
    moved.extend(world.removed::<Parent>());

    // Roots: no parent, or a parent without a transform.
    let mut stack: Vec<(Entity, Option<GlobalTransform>, bool)> = world
        .query_ref_filtered::<(Entity, Option<&Parent>), With<Transform>>()
        .filter(|(_, parent)| parent.is_none_or(|p| !world.has::<Transform>(p.get())))
        .map(|(e, _)| (e, None, false))
        .collect();

    // Depth first; each entry carries the parent's global transform (`None`: the world) and
    // whether it was rewritten.
    while let Some((e, parent, parent_moved)) = stack.pop() {
        let Some(&local) = world.get::<Transform>(e) else {
            continue;
        };
        let dirty = parent_moved || moved.contains(&e);
        let global = if dirty {
            let g = match parent {
                Some(p) => p.mul_transform(&local),
                None => GlobalTransform::from(local),
            };
            if let Some(mut slot) = world.get_mut::<GlobalTransform>(e) {
                *slot = g;
            }
            g
        } else {
            match world.get::<GlobalTransform>(e) {
                Some(g) => *g,
                None => continue,
            }
        };
        if let Some(children) = world.get::<Children>(e) {
            stack.extend(
                children
                    .as_slice()
                    .iter()
                    .rev()
                    .map(|&c| (c, Some(global), dirty)),
            );
        }
    }
}
