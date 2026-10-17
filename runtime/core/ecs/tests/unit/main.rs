//! marlin-ecs's tests (quick tier, docs/design/0014-testing.md §5): worlds, storage, queries,
//! change detection, systems and schedules, checked against plain models. Platform-independent.
//! Run alone with `cargo test -p marlin-ecs --test unit`.

#[path = "../support/mod.rs"]
mod support;

mod change;
mod commands;
mod dynamic;
mod events;
mod hierarchy;
mod hooks;
mod non_send;
mod query;
mod query_state;
mod schedule;
mod storage;
mod systems;
mod world;
