//! Level and Prefab cooking (docs/design/0019-engine-framework-and-world.md §5.3, §5.4).
//!
//! Both cook to a [`CookedLevel`]: every Prefab instance expanded and its overrides applied
//! ([`marlin_world::flatten`]), entities grouped by component set. Prefabs are build
//! dependencies (`build_dependency_types`): the cook reads them, nested ones too, so editing a
//! Prefab recooks every level and Prefab that places it.
//!
//! What the components mean is not the cooker's business, but their structure is part of the
//! output: [`register`] gives the cookers the world's component registry, puts the saved
//! component types in the text schema (so `Light { … }` reads in an entity's component list) and
//! keys every cook on the components' reflected structure, so a changed component recooks.

use std::sync::Arc;

use marlin_asset::Uuid;
use marlin_asset::cooked::Encoding;
use marlin_containers::{HashMap, HashMapExt, HashSet, HashSetExt};
use marlin_cook_core::{
    CookContext, Cooked, Cooker, Cookers, Platform, Profile, SourceObject, fingerprint,
};
use marlin_masset::SchemaError;
use marlin_reflect::{Ref, type_info};
use marlin_world::{
    ComponentFlags, ComponentRegistry, CookedLevel, EntityGuid, FlatEntity, Level, Prefab,
    PrefabInstance, flatten,
};

/// Registers the `Level` and `Prefab` cookers and the saved component types of `components`.
///
/// # Errors
/// A component's text name clashes with another registered type's.
pub fn register(
    cookers: &mut Cookers,
    components: Arc<ComponentRegistry>,
) -> Result<(), SchemaError> {
    let mut key = 0u128;
    for c in components.iter() {
        let Some(info) = c.reflect else { continue };
        if !c.flags.contains(ComponentFlags::SAVED) {
            continue;
        }
        cookers
            .schema_mut()
            .register_info(info, info.attrs.version)?;
        key = key.rotate_left(7) ^ u128::from(fingerprint(info));
    }
    cookers.register::<Level>(LevelCooker {
        kind: Kind::Level,
        key,
    })?;
    cookers.register::<Prefab>(LevelCooker {
        kind: Kind::Prefab,
        key,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Level,
    Prefab,
}

/// Cooks a `Level` or a `Prefab` to a [`CookedLevel`].
struct LevelCooker {
    kind: Kind,
    /// The saved components' structure.
    key: u128,
}

impl Cooker for LevelCooker {
    fn version(&self) -> u32 {
        1
    }

    fn key(&self, _platform: &Platform, _profile: Profile) -> u128 {
        self.key
    }

    fn build_dependency_types(&self) -> &[&str] {
        &["Prefab"]
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let (entities, instances) = match self.kind {
            Kind::Level => {
                let l = object
                    .value
                    .as_ref()
                    .downcast::<Level>()
                    .ok_or("not a Level")?;
                (&l.entities, &l.instances)
            }
            Kind::Prefab => {
                let p = object
                    .value
                    .as_ref()
                    .downcast::<Prefab>()
                    .ok_or("not a Prefab")?;
                (&p.entities, &p.instances)
            }
        };
        let prefabs = read_prefabs(instances, ctx)?;
        let flat = flatten(entities, instances, &prefabs).map_err(|e| e.to_string())?;
        warn_dangling_parents(&flat, ctx);
        let root = match self.kind {
            Kind::Level => EntityGuid::NIL,
            Kind::Prefab => {
                let roots: Vec<EntityGuid> = flat
                    .iter()
                    .filter(|e| e.parent.is_nil())
                    .map(|e| e.guid)
                    .collect();
                match roots[..] {
                    [root] => root,
                    _ => {
                        return Err(format!(
                            "a Prefab needs exactly one root entity; this one has {}",
                            roots.len()
                        ));
                    }
                }
            }
        };
        let cooked = CookedLevel::from_flat(flat, root);
        let mut payload = Vec::new();
        marlin_serial::write(Ref::new(&cooked), &mut payload).map_err(|e| format!("{e:?}"))?;
        Ok(Cooked {
            type_id: type_info::<CookedLevel>().id,
            encoding: Encoding::Serial,
            payload,
        })
    }
}

/// Every Prefab `instances` place, nested ones included, read as build dependencies.
fn read_prefabs(
    instances: &[PrefabInstance],
    ctx: &mut CookContext<'_>,
) -> Result<HashMap<Uuid, Prefab>, String> {
    let mut prefabs: HashMap<Uuid, Prefab> = HashMap::new();
    let mut pending: Vec<Uuid> = instances.iter().map(|i| i.prefab.target).collect();
    while let Some(uuid) = pending.pop() {
        if prefabs.contains_key(&uuid) {
            continue;
        }
        let dependency = ctx.dependency(uuid)?;
        let prefab = dependency
            .value
            .as_ref()
            .downcast::<Prefab>()
            .ok_or_else(|| format!("{uuid} is a `{}`, not a Prefab", dependency.type_name))?
            .clone();
        pending.extend(prefab.instances.iter().map(|i| i.prefab.target));
        prefabs.insert(uuid, prefab);
    }
    Ok(prefabs)
}

/// Parents outside the level are left unparented at runtime: say so now.
fn warn_dangling_parents(flat: &[FlatEntity], ctx: &mut CookContext<'_>) {
    let mut guids = HashSet::new();
    guids.extend(flat.iter().map(|e| e.guid));
    for e in flat {
        if !e.parent.is_nil() && !guids.contains(&e.parent) {
            ctx.warn(format!(
                "`{}` ({:?}) has parent {:?}, which is not in this level",
                e.name, e.guid, e.parent
            ));
        }
    }
}
