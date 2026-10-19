//! Stable entity identity: [`EntityGuid`], references by it ([`EntityRef`]) and the world's
//! index from one to the live [`Entity`] ([`GuidIndex`]) (0019 §5.2).

use core::fmt;

use marlin_containers::{HashMap, HashMapExt};
use marlin_ecs::{Entity, HookContext};
use marlin_reflect::Reflect;

/// An entity's identity in data: the same across saves, cooks and runs, unlike [`Entity`],
/// which only lasts as long as its world. Authored entities get a random one; the entities a
/// Prefab instance makes get [`derive`](Self::derive)d ones.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Reflect)]
#[reflect(name = "marlin_world::EntityGuid")]
pub struct EntityGuid(pub u128);

impl EntityGuid {
    pub const NIL: Self = Self(0);

    pub const fn is_nil(self) -> bool {
        self.0 == 0
    }

    /// The guid of the entity `inner` (a guid inside a Prefab) in the Prefab instance
    /// `instance`. Nested instances derive again from the derived guid.
    ///
    /// Cooked data stores derived guids, so this function is part of the data format: it must
    /// never change.
    pub const fn derive(instance: Self, inner: Self) -> Self {
        let (ih, il) = ((instance.0 >> 64) as u64, instance.0 as u64);
        let (nh, nl) = ((inner.0 >> 64) as u64, inner.0 as u64);
        let lo = mix(il ^ mix(nl ^ 0x9e37_79b9_7f4a_7c15));
        let hi = mix(ih ^ mix(nh ^ 0xc2b2_ae3d_27d4_eb4f) ^ lo);
        Self(((hi as u128) << 64) | lo as u128)
    }
}

/// The splitmix64 finalizer.
const fn mix(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

impl fmt::Debug for EntityGuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EntityGuid({:032x})", self.0)
    }
}

/// A reference to another entity in data, by its [`EntityGuid`]; [`resolve`](Self::resolve)
/// finds it in a world. Nil refers to nothing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug, Reflect)]
#[reflect(name = "marlin_world::EntityRef")]
pub struct EntityRef(pub EntityGuid);

impl EntityRef {
    pub const NONE: Self = Self(EntityGuid::NIL);

    /// The entity, if one with the guid is in the world (its sublevel may not be loaded).
    pub fn resolve(self, index: &GuidIndex) -> Option<Entity> {
        index.get(self.0)
    }
}

/// The world's map from [`EntityGuid`] to [`Entity`], a resource kept current by the
/// `EntityGuid` component's hooks: it reflects a spawn, insert, remove or despawn as soon as that
/// operation returns. When two live entities claim one guid (a data error), the first keeps it
/// and the second is logged.
#[derive(Default)]
pub struct GuidIndex {
    map: HashMap<EntityGuid, Entity>,
}

impl GuidIndex {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn get(&self, guid: EntityGuid) -> Option<Entity> {
        self.map.get(&guid).copied()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    fn insert(&mut self, guid: EntityGuid, entity: Entity) {
        if guid.is_nil() {
            return;
        }
        match self.map.get(&guid) {
            Some(&other) if other != entity => {
                marlin_diag::warn!("{guid:?} is on {other:?} already; {entity:?} is not indexed");
            }
            _ => {
                self.map.insert(guid, entity);
            }
        }
    }

    fn remove(&mut self, guid: EntityGuid, entity: Entity) {
        if self.map.get(&guid) == Some(&entity) {
            self.map.remove(&guid);
        }
    }
}

/// `on_insert`: index the new value.
pub(crate) fn on_insert(cx: &mut HookContext<'_, '_>) {
    let entity = cx.entity;
    let Some(&guid) = cx.world.get::<EntityGuid>(entity) else {
        return;
    };
    cx.commands.add(move |world| {
        if let Some(index) = world.resource_mut::<GuidIndex>() {
            index.insert(guid, entity);
        }
    });
}

/// `on_replace` (before an overwrite or a removal): unindex the old value.
pub(crate) fn on_replace(cx: &mut HookContext<'_, '_>) {
    let entity = cx.entity;
    let Some(&guid) = cx.world.get::<EntityGuid>(entity) else {
        return;
    };
    cx.commands.add(move |world| {
        if let Some(index) = world.resource_mut::<GuidIndex>() {
            index.remove(guid, entity);
        }
    });
}
