//! Levels and Prefabs as authored, their flattening into plain entities, and the cooked form a
//! world spawns from (0019 §5.3, §5.4).
//!
//! - [`Level`] and [`Prefab`] hold [`EntityDesc`]s (an entity: guid, name, parent, components)
//!   and [`PrefabInstance`]s (a Prefab placed with a transform and [`Override`]s).
//! - [`flatten`] expands every instance, nested ones first, into [`FlatEntity`]s: no Prefab
//!   links or overrides are left, and the entities a Prefab instance makes get guids derived from
//!   the instance's ([`EntityGuid::derive`]). The cooker and the editor's play-in-editor share it.
//! - [`CookedLevel::from_flat`] groups entities by component set: each archetype's components
//!   are stored by column, so spawning one entity is one archetype move.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use marlin_asset::{Link, Uuid};
use marlin_containers::{HashMap, HashMapExt, HashSet, HashSetExt};
use marlin_reflect::{Reflect, TypeInfo};

use crate::component_data::ComponentData;
use crate::guid::EntityGuid;
use crate::transform::Transform;

/// An entity as authored.
#[derive(Clone, Default, Debug, Reflect)]
#[reflect(name = "marlin_world::EntityDesc", default)]
pub struct EntityDesc {
    pub guid: EntityGuid,
    pub name: String,
    /// The parent's guid, in the same level or Prefab; nil for none.
    pub parent: EntityGuid,
    pub components: Vec<ComponentData>,
}

/// A Prefab placed in a level or another Prefab. Its root entity takes the instance's guid,
/// parent and transform (and name, if given); the others get guids derived from the instance's.
#[derive(Clone, Default, Debug, Reflect)]
#[reflect(name = "marlin_world::PrefabInstance", default)]
pub struct PrefabInstance {
    pub guid: EntityGuid,
    /// Replaces the root's name when not empty.
    pub name: String,
    pub parent: EntityGuid,
    pub prefab: Link<Prefab>,
    /// The root's transform, replacing the Prefab's.
    pub transform: Transform,
    /// Applied in order, after the Prefab's own instances' overrides.
    pub overrides: Vec<Override>,
}

/// A change an instance makes to its Prefab's entities. `entity` is a guid as the Prefab sees
/// it: an entity of its own, or one a nested instance made (a derived guid).
#[derive(Clone, Debug, Reflect)]
#[reflect(name = "marlin_world::Override")]
pub enum Override {
    /// Sets the listed fields (reflection paths, `"translation.x"`) of the entity's component of
    /// `component`'s type to `component`'s; all of it when `fields` is empty. A component the
    /// entity lacks is added whole.
    Set {
        entity: EntityGuid,
        component: ComponentData,
        fields: Vec<String>,
    },
    /// Removes a component, named by its reflected name, full or short (`Light`).
    RemoveComponent {
        entity: EntityGuid,
        component: String,
    },
    /// Removes an entity and its descendants.
    RemoveEntity { entity: EntityGuid },
}

impl Default for Override {
    fn default() -> Self {
        Self::RemoveEntity {
            entity: EntityGuid::NIL,
        }
    }
}

/// A level as authored: the entities of a persistent level or a sublevel.
#[derive(Clone, Default, Debug, Reflect)]
#[reflect(name = "marlin_world::Level", default)]
pub struct Level {
    pub entities: Vec<EntityDesc>,
    pub instances: Vec<PrefabInstance>,
}

/// A reusable tree of entities. Exactly one entity has no parent: the root an instance places.
#[derive(Clone, Default, Debug, Reflect)]
#[reflect(name = "marlin_world::Prefab", default)]
pub struct Prefab {
    pub entities: Vec<EntityDesc>,
    pub instances: Vec<PrefabInstance>,
}

/// An entity with every Prefab expanded.
#[derive(Clone, Default, Debug, Reflect)]
#[reflect(name = "marlin_world::FlatEntity", default)]
pub struct FlatEntity {
    pub guid: EntityGuid,
    pub name: String,
    pub parent: EntityGuid,
    pub components: Vec<ComponentData>,
}

impl From<&EntityDesc> for FlatEntity {
    fn from(e: &EntityDesc) -> Self {
        Self {
            guid: e.guid,
            name: e.name.clone(),
            parent: e.parent,
            components: e.components.clone(),
        }
    }
}

impl FlatEntity {
    /// Its component of `info`'s type.
    fn component_mut(&mut self, info: &'static TypeInfo) -> Option<&mut ComponentData> {
        self.components
            .iter_mut()
            .find(|c| c.type_info().is_some_and(|t| t.same_type(info)))
    }
}

/// Where [`flatten`] finds Prefabs by UUID.
pub trait PrefabSource {
    fn prefab(&self, uuid: Uuid) -> Option<&Prefab>;
}

impl PrefabSource for HashMap<Uuid, Prefab> {
    fn prefab(&self, uuid: Uuid) -> Option<&Prefab> {
        self.get(&uuid)
    }
}

/// No Prefabs: for levels without instances.
pub struct NoPrefabs;

impl PrefabSource for NoPrefabs {
    fn prefab(&self, _: Uuid) -> Option<&Prefab> {
        None
    }
}

#[derive(Clone, PartialEq, Eq, Debug, marlin_error::Error)]
pub enum FlattenError {
    #[error("instance {instance:?} places Prefab {prefab}, which is missing")]
    MissingPrefab { instance: EntityGuid, prefab: Uuid },
    #[error("Prefab {0} contains an instance of itself")]
    Cycle(Uuid),
    #[error("Prefab {prefab} has {roots} root entities; it needs exactly one")]
    Roots { prefab: Uuid, roots: usize },
    #[error("two entities have the guid {0:?}")]
    DuplicateGuid(EntityGuid),
    #[error("{entity:?} has two `{component}` components")]
    DuplicateComponent {
        entity: EntityGuid,
        component: String,
    },
    #[error("an override of instance {instance:?} names {entity:?}, which its Prefab lacks")]
    OverrideTarget {
        instance: EntityGuid,
        entity: EntityGuid,
    },
    #[error("an override of instance {instance:?} sets `{path}`, which `{component}` lacks")]
    OverridePath {
        instance: EntityGuid,
        component: String,
        path: String,
    },
    #[error("an override of instance {instance:?} sets an empty component")]
    EmptyOverride { instance: EntityGuid },
}

/// Expands `entities` and every instance in `instances` (recursively) into plain entities.
///
/// # Errors
/// A Prefab is missing or contains itself, a Prefab has other than one root, two entities share
/// a guid, an entity has two components of one type, or an override names an entity, component
/// or field that is not there.
pub fn flatten(
    entities: &[EntityDesc],
    instances: &[PrefabInstance],
    prefabs: &dyn PrefabSource,
) -> Result<Vec<FlatEntity>, FlattenError> {
    let mut stack = Vec::new();
    let out = flatten_in(entities, instances, prefabs, &mut stack)?;
    let mut seen = HashSet::new();
    for e in &out {
        if !seen.insert(e.guid) {
            return Err(FlattenError::DuplicateGuid(e.guid));
        }
        let mut types: Vec<u64> = e
            .components
            .iter()
            .filter_map(|c| Some(c.type_info()?.id.0))
            .collect();
        types.sort_unstable();
        if let Some(w) = types.windows(2).find(|w| w[0] == w[1]) {
            let info = marlin_reflect::find(marlin_reflect::TypeId(w[0]));
            return Err(FlattenError::DuplicateComponent {
                entity: e.guid,
                component: info.map_or_else(String::new, |i| i.name.to_string()),
            });
        }
    }
    Ok(out)
}

/// A Prefab flattened in its own guid space.
pub fn flatten_prefab(
    uuid: Uuid,
    prefabs: &dyn PrefabSource,
) -> Result<Vec<FlatEntity>, FlattenError> {
    let mut stack = Vec::new();
    expand_prefab(uuid, EntityGuid::NIL, prefabs, &mut stack)
}

fn flatten_in(
    entities: &[EntityDesc],
    instances: &[PrefabInstance],
    prefabs: &dyn PrefabSource,
    stack: &mut Vec<Uuid>,
) -> Result<Vec<FlatEntity>, FlattenError> {
    let mut out: Vec<FlatEntity> = entities.iter().map(FlatEntity::from).collect();
    for instance in instances {
        out.extend(expand_instance(instance, prefabs, stack)?);
    }
    Ok(out)
}

fn expand_prefab(
    uuid: Uuid,
    instance: EntityGuid,
    prefabs: &dyn PrefabSource,
    stack: &mut Vec<Uuid>,
) -> Result<Vec<FlatEntity>, FlattenError> {
    if stack.contains(&uuid) {
        return Err(FlattenError::Cycle(uuid));
    }
    let prefab = prefabs.prefab(uuid).ok_or(FlattenError::MissingPrefab {
        instance,
        prefab: uuid,
    })?;
    stack.push(uuid);
    let result = flatten_in(&prefab.entities, &prefab.instances, prefabs, stack);
    stack.pop();
    result
}

fn expand_instance(
    instance: &PrefabInstance,
    prefabs: &dyn PrefabSource,
    stack: &mut Vec<Uuid>,
) -> Result<Vec<FlatEntity>, FlattenError> {
    let uuid = instance.prefab.target;
    let mut local = expand_prefab(uuid, instance.guid, prefabs, stack)?;
    for o in &instance.overrides {
        apply_override(&mut local, o, instance.guid)?;
    }

    let roots: Vec<EntityGuid> = local
        .iter()
        .filter(|e| e.parent.is_nil())
        .map(|e| e.guid)
        .collect();
    let [root] = roots[..] else {
        return Err(FlattenError::Roots {
            prefab: uuid,
            roots: roots.len(),
        });
    };
    let remap = |g: EntityGuid| {
        if g == root {
            instance.guid
        } else {
            EntityGuid::derive(instance.guid, g)
        }
    };
    for e in &mut local {
        let is_root = e.guid == root;
        e.guid = remap(e.guid);
        if is_root {
            e.parent = instance.parent;
            if !instance.name.is_empty() {
                e.name.clone_from(&instance.name);
            }
            let info = marlin_reflect::type_info::<Transform>();
            match e.component_mut(info) {
                Some(t) => *t = ComponentData::new(instance.transform),
                None => e.components.push(ComponentData::new(instance.transform)),
            }
        } else {
            e.parent = remap(e.parent);
        }
    }
    Ok(local)
}

fn apply_override(
    local: &mut Vec<FlatEntity>,
    o: &Override,
    instance: EntityGuid,
) -> Result<(), FlattenError> {
    let target = |local: &[FlatEntity], entity: EntityGuid| {
        local
            .iter()
            .position(|e| e.guid == entity)
            .ok_or(FlattenError::OverrideTarget { instance, entity })
    };
    match o {
        Override::Set {
            entity,
            component,
            fields,
        } => {
            let i = target(local, *entity)?;
            let (Some(info), Some(src)) = (component.type_info(), component.get()) else {
                return Err(FlattenError::EmptyOverride { instance });
            };
            let e = &mut local[i];
            let Some(existing) = e.component_mut(info) else {
                e.components.push(component.clone());
                return Ok(());
            };
            if fields.is_empty() {
                *existing = component.clone();
                return Ok(());
            }
            let mut dst = existing.get_mut().expect("has a value of the type");
            for path in fields {
                let path_error = || FlattenError::OverridePath {
                    instance,
                    component: info.name.to_string(),
                    path: path.clone(),
                };
                let from = src.path(path).map_err(|_| path_error())?;
                let mut to = dst.reborrow().path(path).map_err(|_| path_error())?;
                if !to.assign(from) {
                    return Err(path_error());
                }
            }
        }
        Override::RemoveComponent { entity, component } => {
            let i = target(local, *entity)?;
            local[i].components.retain(|c| {
                c.type_info().is_none_or(|t| {
                    let full = t.name.to_string();
                    full != *component && short_name(&full) != component
                })
            });
        }
        Override::RemoveEntity { entity } => {
            target(local, *entity)?;
            let mut gone = HashSet::new();
            gone.insert(*entity);
            // Descendants, whatever their order in the list.
            loop {
                let before = gone.len();
                for e in local.iter() {
                    if gone.contains(&e.parent) {
                        gone.insert(e.guid);
                    }
                }
                if gone.len() == before {
                    break;
                }
            }
            local.retain(|e| !gone.contains(&e.guid));
        }
    }
    Ok(())
}

fn short_name(full: &str) -> &str {
    full.rsplit("::").next().unwrap_or(full)
}

/// A cooked entity: its identity and place in the hierarchy; its components are in its
/// archetype's columns.
#[derive(Clone, Default, Debug, Reflect)]
#[reflect(name = "marlin_world::CookedEntity", default)]
pub struct CookedEntity {
    pub guid: EntityGuid,
    pub name: String,
    pub parent: EntityGuid,
}

/// Entities with the same component types: `columns[c][e]` is entity `e`'s component `c`.
#[derive(Clone, Default, Debug, Reflect)]
#[reflect(name = "marlin_world::CookedArchetype", default)]
pub struct CookedArchetype {
    pub entities: Vec<CookedEntity>,
    pub columns: Vec<Vec<ComponentData>>,
}

/// What a level or a Prefab cooks to, and what a world spawns. A Prefab's has the guid of its
/// root (its entities' guids are its own, derived per instance when spawned); a level's root is
/// nil.
#[derive(Clone, Default, Debug, Reflect)]
#[reflect(name = "marlin_world::CookedLevel", default)]
pub struct CookedLevel {
    pub root: EntityGuid,
    pub archetypes: Vec<CookedArchetype>,
}

impl CookedLevel {
    /// Groups flattened entities by their set of component types, keeping their order within
    /// each group. Empty components are dropped.
    pub fn from_flat(entities: Vec<FlatEntity>, root: EntityGuid) -> Self {
        let mut index: HashMap<Vec<u64>, usize> = HashMap::new();
        let mut archetypes: Vec<CookedArchetype> = Vec::new();
        for mut e in entities {
            e.components.retain(|c| !c.is_none());
            e.components
                .sort_by_key(|c| c.type_info().map_or(0, |t| t.id.0));
            let key: Vec<u64> = e
                .components
                .iter()
                .map(|c| c.type_info().map_or(0, |t| t.id.0))
                .collect();
            let a = *index.entry(key).or_insert_with(|| {
                archetypes.push(CookedArchetype {
                    entities: Vec::new(),
                    columns: e.components.iter().map(|_| Vec::new()).collect(),
                });
                archetypes.len() - 1
            });
            let archetype = &mut archetypes[a];
            archetype.entities.push(CookedEntity {
                guid: e.guid,
                name: e.name,
                parent: e.parent,
            });
            for (column, c) in archetype.columns.iter_mut().zip(e.components) {
                column.push(c);
            }
        }
        Self { root, archetypes }
    }

    pub fn entity_count(&self) -> usize {
        self.archetypes.iter().map(|a| a.entities.len()).sum()
    }

    /// Components that read back empty: the cook drops empty ones, so these named a type this
    /// build does not have (cooked data older than its components, or another build's).
    pub fn unknown_components(&self) -> usize {
        self.archetypes
            .iter()
            .flat_map(|a| a.columns.iter().flatten())
            .filter(|c| c.is_none())
            .count()
    }
}
