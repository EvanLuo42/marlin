//! The levels in a world: the persistent level and sublevels, spawned from [`CookedLevel`]s
//! and unloaded again (0019 §5.4).
//!
//! A sublevel goes through three states. [`World::load_sublevel`] names a level asset and
//! returns its [`SublevelId`]; the host loads the asset (it owns the asset server) and hands it
//! over with [`World::provide_level`]; the world then spawns it in `FrameStart`, a slice of
//! entities a frame within `w.spawn_budget_ms`. [`World::spawn_level`] spawns a loaded level at
//! once instead (the persistent level at startup, tests, play in editor).

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_asset::Uuid;
use marlin_config::CvarF64;
use marlin_containers::{HashMap, HashMapExt};
use marlin_ecs::Entity;
use marlin_reflect::{Ref, Reflect};
use marlin_time::Instant;

use crate::guid::{EntityGuid, GuidIndex};
use crate::level::CookedLevel;
use crate::transform::Transform;

pub static W_SPAWN_BUDGET_MS: CvarF64 = CvarF64::new(
    "w.spawn_budget_ms",
    2.0,
    "Time a frame may spend spawning streamed sublevels, in milliseconds.",
)
.range(0.05, 1000.0);

/// A level loaded (or loading) in a world. The persistent level is the first one spawned.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Debug, Reflect)]
#[reflect(name = "marlin_world::SublevelId")]
pub struct SublevelId(pub u32);

/// The sublevel an entity was spawned with; unloading it despawns the entity.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Reflect)]
#[reflect(name = "marlin_world::InLevel")]
pub struct InLevel(pub SublevelId);

/// An entity's name as authored, for tools and logs.
#[derive(Clone, PartialEq, Eq, Default, Debug, Reflect)]
#[reflect(name = "marlin_world::EntityName", default)]
pub struct EntityName(pub String);

/// Where a sublevel is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SublevelState {
    /// Waiting for the host to load the asset.
    Requested,
    /// Spawning, a slice a frame.
    Spawning,
    Loaded,
}

/// A level asset a sublevel wants, for the host to load and [`provide`](World::provide_level).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LevelRequest {
    pub id: SublevelId,
    pub level: Uuid,
}

struct Sublevel {
    asset: Uuid,
    placement: Transform,
    state: SublevelState,
    data: Option<Arc<CookedLevel>>,
    /// The next entity to spawn: archetype, then entity in it.
    cursor: (usize, usize),
    entities: Vec<Entity>,
    /// Children spawned before their parents: (child, parent guid).
    orphans: Vec<(Entity, EntityGuid)>,
}

/// The world's sublevels, a resource.
#[derive(Default)]
pub struct Levels {
    next: u32,
    sublevels: HashMap<SublevelId, Sublevel>,
}

impl Levels {
    pub fn new() -> Self {
        Self {
            next: 0,
            sublevels: HashMap::new(),
        }
    }

    pub fn state(&self, id: SublevelId) -> Option<SublevelState> {
        self.sublevels.get(&id).map(|s| s.state)
    }

    /// Level assets waiting to be provided.
    pub fn requests(&self) -> impl Iterator<Item = LevelRequest> + '_ {
        self.sublevels
            .iter()
            .filter(|(_, s)| s.state == SublevelState::Requested)
            .map(|(&id, s)| LevelRequest { id, level: s.asset })
    }

    /// The entities a sublevel has spawned so far.
    pub fn entities(&self, id: SublevelId) -> &[Entity] {
        self.sublevels.get(&id).map_or(&[], |s| &s.entities)
    }

    fn add(
        &mut self,
        asset: Uuid,
        placement: Transform,
        data: Option<Arc<CookedLevel>>,
    ) -> SublevelId {
        let id = SublevelId(self.next);
        self.next += 1;
        self.sublevels.insert(
            id,
            Sublevel {
                asset,
                placement,
                state: if data.is_some() {
                    SublevelState::Spawning
                } else {
                    SublevelState::Requested
                },
                data,
                cursor: (0, 0),
                entities: Vec::new(),
                orphans: Vec::new(),
            },
        );
        id
    }
}

/// Spawns entities of `sub` until it is done or `deadline` passes (checked every few entities);
/// true when done.
fn spawn_slice(
    ecs: &mut marlin_ecs::World,
    id: SublevelId,
    sub: &mut Sublevel,
    deadline: Option<Instant>,
) -> bool {
    let Some(data) = sub.data.clone() else {
        return false;
    };
    let identity = sub.placement == Transform::IDENTITY;
    let mut spawned = 0u32;
    while let Some(archetype) = data.archetypes.get(sub.cursor.0) {
        let Some(cooked) = archetype.entities.get(sub.cursor.1) else {
            sub.cursor = (sub.cursor.0 + 1, 0);
            continue;
        };
        if let Some(deadline) = deadline
            && spawned % 16 == 0
            && spawned > 0
            && Instant::now() > deadline
        {
            return false;
        }
        let row = sub.cursor.1;
        sub.cursor.1 += 1;
        spawned += 1;

        let in_level = InLevel(id);
        let name = EntityName(cooked.name.clone());
        let mut placed: Option<Transform> = None;
        let mut values: Vec<Ref<'_>> = Vec::with_capacity(archetype.columns.len() + 3);
        for column in &archetype.columns {
            let Some(value) = column.get(row).and_then(|c| c.get()) else {
                continue;
            };
            // Roots move with the sublevel's placement.
            if !identity
                && cooked.parent.is_nil()
                && let Some(t) = value.downcast::<Transform>()
            {
                placed = Some(sub.placement.mul_transform(t));
                continue;
            }
            values.push(value);
        }
        if let Some(t) = &placed {
            values.push(Ref::new(t));
        }
        values.push(Ref::new(&cooked.guid));
        values.push(Ref::new(&in_level));
        if !cooked.name.is_empty() {
            values.push(Ref::new(&name));
        }
        let e = ecs.spawn_empty();
        if !ecs.insert_cloned(e, &values) {
            marlin_diag::warn!(
                "{:?} ({}) has a component this world does not know; spawned without its components",
                cooked.guid,
                cooked.name
            );
            ecs.insert(e, cooked.guid);
            ecs.insert(e, in_level);
        }
        sub.entities.push(e);
        if !cooked.parent.is_nil() {
            let parent = ecs
                .resource::<GuidIndex>()
                .and_then(|i| i.get(cooked.parent));
            match parent {
                Some(p) => {
                    ecs.set_parent(e, p);
                }
                None => sub.orphans.push((e, cooked.parent)),
            }
        }
    }
    // Everything is spawned: parents spawned after their children.
    for (child, parent) in core::mem::take(&mut sub.orphans) {
        match ecs.resource::<GuidIndex>().and_then(|i| i.get(parent)) {
            Some(p) => {
                ecs.set_parent(child, p);
            }
            None => marlin_diag::warn!("{child:?}'s parent {parent:?} is not in the world"),
        }
    }
    sub.state = SublevelState::Loaded;
    true
}

/// `FrameStart`: spawns sublevels in the order they were provided, within the frame's budget.
pub(crate) fn stream_sublevels(ecs: &mut marlin_ecs::World) {
    let Some(mut levels) = ecs.remove_resource::<Levels>() else {
        return;
    };
    let budget_ns = (W_SPAWN_BUDGET_MS.get() * 1.0e6) as u64;
    let start = Instant::now();
    let deadline = start + core::time::Duration::from_nanos(budget_ns);
    let mut spawning: Vec<SublevelId> = levels
        .sublevels
        .iter()
        .filter(|(_, s)| s.state == SublevelState::Spawning)
        .map(|(&id, _)| id)
        .collect();
    spawning.sort_unstable();
    for id in spawning {
        let sub = levels.sublevels.get_mut(&id).expect("listed");
        if !spawn_slice(ecs, id, sub, Some(deadline)) {
            break;
        }
    }
    ecs.insert_resource(levels);
}

impl crate::World {
    fn levels_mut(&mut self) -> &mut Levels {
        self.ecs_mut()
            .resource_mut::<Levels>()
            .expect("worlds have Levels")
    }

    pub fn levels(&self) -> &Levels {
        self.ecs().resource::<Levels>().expect("worlds have Levels")
    }

    /// Spawns a loaded level now, placed by `placement`, as a new sublevel.
    pub fn spawn_level(&mut self, level: Arc<CookedLevel>, placement: Transform) -> SublevelId {
        let id = self.levels_mut().add(Uuid::NIL, placement, Some(level));
        let ecs = self.ecs_mut();
        let mut levels = ecs.remove_resource::<Levels>().expect("worlds have Levels");
        let sub = levels.sublevels.get_mut(&id).expect("just added");
        spawn_slice(ecs, id, sub, None);
        ecs.insert_resource(levels);
        id
    }

    /// Asks for the level asset `level`, placed by `placement`: the host loads it and calls
    /// [`provide_level`](Self::provide_level); it then spawns over the next frames.
    pub fn load_sublevel(&mut self, level: Uuid, placement: Transform) -> SublevelId {
        self.levels_mut().add(level, placement, None)
    }

    /// The loaded asset of a requested sublevel. False if `id` is not waiting for one (it was
    /// unloaded meanwhile).
    pub fn provide_level(&mut self, id: SublevelId, level: Arc<CookedLevel>) -> bool {
        match self.levels_mut().sublevels.get_mut(&id) {
            Some(sub) if sub.state == SublevelState::Requested => {
                sub.data = Some(level);
                sub.state = SublevelState::Spawning;
                true
            }
            _ => false,
        }
    }

    /// Despawns a sublevel's entities (those still alive) and forgets it, stopping a load in
    /// progress. Returns how many entities it despawned.
    pub fn unload_sublevel(&mut self, id: SublevelId) -> usize {
        let Some(sub) = self.levels_mut().sublevels.remove(&id) else {
            return 0;
        };
        let ecs = self.ecs_mut();
        let mut despawned = 0;
        for e in sub.entities {
            if ecs.get::<InLevel>(e).is_some_and(|l| l.0 == id) && ecs.despawn(e) {
                despawned += 1;
            }
        }
        despawned
    }

    /// Spawns a cooked Prefab now as the instance `guid`: its root takes `guid` and
    /// `transform` (and `parent`, if any); the others get guids derived from `guid`. Belongs
    /// to no sublevel. Returns the root.
    pub fn spawn_prefab(
        &mut self,
        prefab: &CookedLevel,
        guid: EntityGuid,
        transform: Transform,
        parent: Option<Entity>,
    ) -> Option<Entity> {
        let remap = |g: EntityGuid| {
            if g == prefab.root {
                guid
            } else {
                EntityGuid::derive(guid, g)
            }
        };
        let ecs = self.ecs_mut();
        let mut by_guid: HashMap<EntityGuid, Entity> = HashMap::new();
        let mut links: Vec<(Entity, EntityGuid)> = Vec::new();
        let mut root = None;
        for archetype in &prefab.archetypes {
            for (row, cooked) in archetype.entities.iter().enumerate() {
                let is_root = cooked.guid == prefab.root;
                let g = remap(cooked.guid);
                let name = EntityName(cooked.name.clone());
                let mut values: Vec<Ref<'_>> = archetype
                    .columns
                    .iter()
                    .filter_map(|c| c.get(row).and_then(|c| c.get()))
                    .filter(|v| !(is_root && v.downcast::<Transform>().is_some()))
                    .collect();
                if is_root {
                    values.push(Ref::new(&transform));
                }
                values.push(Ref::new(&g));
                if !cooked.name.is_empty() {
                    values.push(Ref::new(&name));
                }
                let e = ecs.spawn_empty();
                if !ecs.insert_cloned(e, &values) {
                    marlin_diag::warn!("{g:?} has a component this world does not know");
                    ecs.insert(e, g);
                }
                by_guid.insert(cooked.guid, e);
                if is_root {
                    root = Some(e);
                } else {
                    links.push((e, cooked.parent));
                }
            }
        }
        for (child, parent) in links {
            if let Some(&p) = by_guid.get(&parent) {
                ecs.set_parent(child, p);
            }
        }
        if let (Some(r), Some(p)) = (root, parent) {
            ecs.set_parent(r, p);
        }
        root
    }
}
