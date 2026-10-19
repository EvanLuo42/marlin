//! marlin-world's tests (quick tier, docs/design/0014-testing.md §5): transform propagation,
//! tick groups and fixed steps, the component registry, entity guids, and levels: Prefab
//! flattening, cooked archetypes, spawning and sublevel streaming. Platform-independent.
//! Run alone with `cargo test -p marlin-world --test unit`.

mod frame;
mod guid;
mod level;
mod module;
mod registry;
mod transform;

use std::sync::Arc;

use marlin_jobs::{Config, JobSystem};
use marlin_world::{ComponentRegistry, TimeSettings, World, WorldKind};

/// A job system for running schedules.
pub fn jobs() -> JobSystem {
    JobSystem::new(Config {
        workers: 2,
        ..Config::default()
    })
    .unwrap()
}

/// A world of `kind` with the world's own components and fixed time settings (60 Hz, at most 4
/// steps, frames clamped to 0.1 s), whatever the cvars say.
pub fn world(kind: WorldKind) -> World {
    let mut world = World::new(kind, Arc::new(ComponentRegistry::new()));
    world.ecs_mut().insert_resource(TimeSettings {
        max_delta: 0.1,
        fixed_step: 1.0 / 60.0,
        max_substeps: 4,
    });
    world
}
