//! Modules: dependency order, registration into the table, worlds made from it, hooks, and the
//! world module loading level assets for sublevels.

use std::sync::{Arc, Mutex};

use marlin_asset::cooked::write::{UnitObject, unit};
use marlin_asset::cooked::{Encoding, platform_id, unit_id};
use marlin_asset::{AssetServer, MemorySource, Uuid, Vfs};
use marlin_ecs::{IntoSystem, ResMut};
use marlin_jobs::JobSystem;
use marlin_reflect::{Reflect, type_info};
use marlin_world::{
    ComponentData, ComponentFlags, CookedLevel, EngineContext, EntityDesc, EntityGuid, Module,
    ModuleError, Modules, NoPrefabs, Registry, Resources, SublevelState, TickGroup, Transform,
    World, WorldKind, WorldModule, flatten,
};

use crate::jobs;

/// A module for tests: a name, dependencies, and a record of its hooks.
struct Probe {
    name: &'static str,
    deps: &'static [&'static str],
    log: Arc<Mutex<Vec<String>>>,
}

#[derive(Default)]
struct Ticks(u32);

#[derive(Clone, Copy, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "module_test::Marker")]
struct Marker;

impl Module for Probe {
    fn name(&self) -> &'static str {
        self.name
    }

    fn dependencies(&self) -> &'static [&'static str] {
        self.deps
    }

    fn register(&self, r: &mut Registry) {
        self.log
            .lock()
            .unwrap()
            .push(format!("register {}", self.name));
        if self.name == "B" {
            r.components().add::<Marker>(ComponentFlags::SAVED);
            r.add_system(TickGroup::PrePhysics, || {
                (|mut t: ResMut<'_, Ticks>| t.0 += 1).named("tick")
            });
        }
    }

    fn startup(&mut self, _: &mut EngineContext<'_, '_>) -> Result<(), ModuleError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("startup {}", self.name));
        if self.name == "Broken" {
            return Err(ModuleError::new(self.name, "no device"));
        }
        Ok(())
    }

    fn init_world(&mut self, world: &mut World, _: &mut EngineContext<'_, '_>) {
        if self.name == "B" {
            world.ecs_mut().insert_resource(Ticks(0));
        }
    }

    fn pre_world_frame(&mut self, _: &mut World, _: &mut EngineContext<'_, '_>) {
        self.log.lock().unwrap().push(format!("pre {}", self.name));
    }

    fn post_world_frame(&mut self, _: &mut World, _: &mut EngineContext<'_, '_>) {
        self.log.lock().unwrap().push(format!("post {}", self.name));
    }
}

fn probe(
    name: &'static str,
    deps: &'static [&'static str],
    log: &Arc<Mutex<Vec<String>>>,
) -> Box<dyn Module> {
    Box::new(Probe {
        name,
        deps,
        log: log.clone(),
    })
}

/// What a module hook gets, made of the given parts.
fn cx<'a, 'v>(
    assets: &'a mut AssetServer,
    vfs: &'v Vfs,
    jobs: &'a JobSystem,
    resources: &'a mut Resources,
) -> EngineContext<'a, 'v> {
    EngineContext {
        assets,
        vfs,
        jobs,
        resources,
    }
}

#[test]
fn modules_run_after_their_dependencies() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut modules = Modules::new(vec![
        probe("A", &["B"], &log),
        probe("B", &[], &log),
        probe("C", &["A"], &log),
    ])
    .unwrap();
    assert_eq!(modules.names().collect::<Vec<_>>(), ["B", "A", "C"]);
    assert!(modules.has("C") && !modules.has("D"));
    assert!(modules.get::<Probe>().is_some(), "found by type");

    let (jobs, vfs) = (jobs(), Vfs::new());
    let mut resources = Resources::new();
    let mut assets = AssetServer::new(modules.table().loaders(&resources));
    modules
        .startup(&mut cx(&mut assets, &vfs, &jobs, &mut resources))
        .unwrap();
    let mut world = modules.create_world(
        WorldKind::Game,
        &mut cx(&mut assets, &vfs, &jobs, &mut resources),
    );
    assert!(world.registry().get::<Marker>().is_some(), "B's component");
    for _ in 0..3 {
        modules.frame(
            &mut world,
            0.016,
            &mut cx(&mut assets, &vfs, &jobs, &mut resources),
        );
    }
    assert_eq!(world.ecs().resource::<Ticks>().unwrap().0, 3, "B's system");
    let log = log.lock().unwrap();
    assert_eq!(
        log[..6],
        [
            "register B",
            "register A",
            "register C",
            "startup B",
            "startup A",
            "startup C"
        ]
    );
    assert_eq!(
        log[6..12],
        ["pre B", "pre A", "pre C", "post B", "post A", "post C"]
    );
}

#[test]
fn bad_module_lists_are_refused() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let err = |list| Modules::new(list).err().unwrap().to_string();
    assert!(err(vec![probe("A", &[], &log), probe("A", &[], &log)]).contains("listed twice"));
    assert!(err(vec![probe("A", &["Gone"], &log)]).contains("`Gone`"));
    assert!(err(vec![probe("A", &["B"], &log), probe("B", &["A"], &log)]).contains("cycle"));

    let mut modules = Modules::new(vec![probe("Broken", &[], &log)]).unwrap();
    let (jobs, vfs) = (jobs(), Vfs::new());
    let mut resources = Resources::new();
    let mut assets = AssetServer::new(modules.table().loaders(&resources));
    let e = modules
        .startup(&mut cx(&mut assets, &vfs, &jobs, &mut resources))
        .unwrap_err();
    assert_eq!(e.to_string(), "module `Broken`: no device");
}

#[derive(Clone, Copy, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "module_test::Weight")]
struct Weight(f32);

/// A test module with `Weight`.
struct Weights;

impl Module for Weights {
    fn name(&self) -> &'static str {
        "Weights"
    }

    fn register(&self, r: &mut Registry) {
        r.components().add::<Weight>(ComponentFlags::SAVED);
    }
}

const LEVEL: Uuid = Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_1e7e);

/// Content holding one level of two entities.
fn content() -> Vfs {
    let entities = [
        EntityDesc {
            guid: EntityGuid::from_u128(1),
            name: "Heavy".into(),
            components: vec![
                ComponentData::new(Transform::IDENTITY),
                ComponentData::new(Weight(9.0)),
            ],
            ..EntityDesc::default()
        },
        EntityDesc {
            guid: EntityGuid::from_u128(2),
            name: "Plain".into(),
            components: vec![ComponentData::new(Transform::IDENTITY)],
            ..EntityDesc::default()
        },
    ];
    let level = CookedLevel::from_flat(
        flatten(&entities, &[], &NoPrefabs).unwrap(),
        EntityGuid::NIL,
    );
    let bytes = unit(
        unit_id("levels/main"),
        platform_id("windows"),
        vec![UnitObject {
            uuid: LEVEL,
            type_id: type_info::<CookedLevel>().id.0,
            encoding: Encoding::Serial,
            payload: marlin_serial::to_bytes(&level).unwrap(),
            bulk: Vec::new(),
        }],
        &[],
    );
    let mut source = MemorySource::new();
    source.insert(bytes).unwrap();
    let mut vfs = Vfs::new();
    vfs.mount("game", source);
    vfs
}

#[test]
fn the_world_module_loads_sublevels_from_assets() {
    // The registry must know `Weight` before the level is read.
    let mut modules = Modules::new(vec![Box::new(WorldModule), Box::new(Weights)]).unwrap();
    let (jobs, vfs) = (jobs(), content());
    let mut resources = Resources::new();
    let mut assets = AssetServer::new(modules.table().loaders(&resources));
    let mut world = modules.create_world(
        WorldKind::Game,
        &mut cx(&mut assets, &vfs, &jobs, &mut resources),
    );
    let id = world.load_sublevel(LEVEL, Transform::IDENTITY);
    assert!(!modules.world_ready(&world));
    let mut frames = 0;
    while world.levels().state(id) != Some(SublevelState::Loaded) {
        assets.update_blocking(&vfs, Some(&jobs));
        modules.frame(
            &mut world,
            0.016,
            &mut cx(&mut assets, &vfs, &jobs, &mut resources),
        );
        frames += 1;
        assert!(frames < 8, "the level did not load");
    }
    assert!(modules.world_ready(&world));
    let heavy = world.entity_by_guid(EntityGuid::from_u128(1)).unwrap();
    assert_eq!(world.ecs().get::<Weight>(heavy), Some(&Weight(9.0)));
    assert!(world.entity_by_guid(EntityGuid::from_u128(2)).is_some());

    // A missing level unloads its sublevel.
    let missing = world.load_sublevel(Uuid::from_u128(0xdead), Transform::IDENTITY);
    for _ in 0..3 {
        assets.update_blocking(&vfs, Some(&jobs));
        modules.frame(
            &mut world,
            0.016,
            &mut cx(&mut assets, &vfs, &jobs, &mut resources),
        );
    }
    assert_eq!(world.levels().state(missing), None);
    assert_eq!(world.levels().state(id), Some(SublevelState::Loaded));
}
