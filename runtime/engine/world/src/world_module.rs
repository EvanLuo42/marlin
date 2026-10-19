//! [`WorldModule`]: the world's own module. It loads the level assets sublevels ask for, hands
//! them to their worlds, and spawns a level again when its asset reloads (live editing).

use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_asset::{Handle, LoadState, Uuid};
use marlin_containers::{HashMap, HashMapExt};

use crate::level::CookedLevel;
use crate::levels::{SublevelId, SublevelState, W_SPAWN_BUDGET_MS};
use crate::module::{EngineContext, Module, Registry};
use crate::time::{T_FIXED_HZ, T_MAX_DT, T_MAX_SUBSTEPS};
use crate::world::World;

/// The world module: levels from assets (0019 §5.4).
pub struct WorldModule;

impl WorldModule {
    pub const NAME: &'static str = "World";
}

/// A world's level loads (a world resource).
#[derive(Default)]
struct LevelLoads {
    /// By sublevel: the asset and the version handed over.
    loads: HashMap<SublevelId, (Uuid, Handle<CookedLevel>, Option<u32>)>,
}

impl Module for WorldModule {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn register(&self, r: &mut Registry) {
        r.add_loaders(|loaders, _| loaders.register_serial::<CookedLevel>())
            .add_cvars(&[&T_MAX_DT, &T_FIXED_HZ, &T_MAX_SUBSTEPS, &W_SPAWN_BUDGET_MS]);
    }

    fn init_world(&mut self, world: &mut World, _: &mut EngineContext<'_, '_>) {
        world.ecs_mut().insert_resource(LevelLoads {
            loads: HashMap::new(),
        });
    }

    fn pre_world_frame(&mut self, world: &mut World, cx: &mut EngineContext<'_, '_>) {
        let Some(mut loads) = world.ecs_mut().remove_resource::<LevelLoads>() else {
            return;
        };
        // New requests.
        let requests: Vec<_> = world.levels().requests().collect();
        for r in requests {
            loads
                .loads
                .entry(r.id)
                .or_insert_with(|| (r.level, cx.assets.load(r.level), None));
        }
        // Unloaded sublevels let go of their asset.
        loads
            .loads
            .retain(|&id, _| world.levels().state(id).is_some());

        let ids: Vec<SublevelId> = loads.loads.keys().copied().collect();
        let mut reloads = Vec::new();
        for id in ids {
            let (asset, handle, provided) = loads.loads.get_mut(&id).expect("listed");
            match cx.assets.state(handle) {
                LoadState::Loading => continue,
                LoadState::Failed(e) => {
                    marlin_diag::warn!("level {asset}: {e:?}; its sublevel is unloaded");
                    world.unload_sublevel(id);
                    continue;
                }
                LoadState::Loaded => {}
            }
            let version = cx.assets.version(handle);
            match world.levels().state(id) {
                Some(SublevelState::Requested) => {
                    if let Some(level) = cx.assets.get(handle) {
                        world.provide_level(id, Arc::new(level.clone()));
                        *provided = Some(version);
                    }
                }
                Some(_) if *provided != Some(version) => reloads.push(id),
                _ => {}
            }
        }
        // A level reloaded (its source edited): spawned again where it was.
        for id in reloads {
            let Some((asset, handle, _)) = loads.loads.remove(&id) else {
                continue;
            };
            let placement = world.levels().placement(id).unwrap_or_default();
            world.unload_sublevel(id);
            let new = world.load_sublevel(asset, placement);
            let version = cx.assets.version(&handle);
            if let Some(level) = cx.assets.get(&handle) {
                world.provide_level(new, Arc::new(level.clone()));
            }
            loads.loads.insert(new, (asset, handle, Some(version)));
        }
        world.ecs_mut().insert_resource(loads);
    }

    fn world_ready(&self, world: &World) -> bool {
        world.levels().all_loaded()
    }
}
