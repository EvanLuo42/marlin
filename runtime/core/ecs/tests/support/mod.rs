//! Helpers shared by marlin-ecs's test binaries.
#![allow(dead_code)]

use marlin_jobs::{Config, JobSystem};

/// A job system with `workers` workers and the default fiber settings, for running schedules.
pub fn jobs(workers: usize) -> JobSystem {
    JobSystem::new(Config {
        workers,
        ..Config::default()
    })
    .unwrap()
}
