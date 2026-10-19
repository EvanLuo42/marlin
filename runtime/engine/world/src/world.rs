//! [`World`]: an ECS world with its tick schedule, clocks and identity index.

use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_ecs::{Added, Entity, IntoSystem};
use marlin_jobs::JobSystem;
use marlin_profile::profile_scope;

use crate::guid::{self, EntityGuid, GuidIndex};
use crate::registry::ComponentRegistry;
use crate::schedule::{TickGroup, WorldSchedule};
use crate::time::{FixedTime, Interpolated, Time, TimeSettings};
use crate::transform::{Transform, propagate_transforms};

/// What a world is for; decides which tick groups run (0019 §4.1).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum WorldKind {
    /// The running game.
    Game,
    /// The editor's world: placed, drawn, not simulated.
    Editor,
    /// Play in editor: a game world spawned from the editor's.
    Pie,
    /// An asset editor's small scene (a Prefab, an animation), simulated.
    Preview,
}

impl WorldKind {
    /// Runs the gameplay tick groups.
    pub const fn simulates(self) -> bool {
        !matches!(self, Self::Editor)
    }
}

/// The world's part in a networked session (0019 §11; reserved: only `Standalone` is
/// implemented).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum NetRole {
    #[default]
    Standalone,
    Server,
    Client,
}

/// A world: entities and their components, the systems that run on them each frame, its clocks
/// and its index of entity guids.
///
/// Each [`frame`](Self::frame) runs the [`TickGroup`]s in order:
///
/// 1. advance [`Time`];
/// 2. `FrameStart`;
/// 3. if the world simulates and is not paused: `PrePhysics`, then the fixed groups once per
///    fixed step [`FixedTime`] grants, then `PostPhysics` and `LateUpdate`;
/// 4. `TransformPropagate`, `Extract`, `FrameEnd`;
/// 5. clear the change trackers: the next frame's "changed" starts empty.
pub struct World {
    ecs: marlin_ecs::World,
    kind: WorldKind,
    role: NetRole,
    schedule: WorldSchedule,
    registry: Arc<ComponentRegistry>,
}

impl World {
    /// A world of `kind` knowing the components of `registry`, with the world's own systems and
    /// resources: [`Time`], [`FixedTime`], [`TimeSettings`] (from the `t.*` cvars) and
    /// [`GuidIndex`].
    pub fn new(kind: WorldKind, registry: Arc<ComponentRegistry>) -> Self {
        let mut ecs = marlin_ecs::World::new();
        for entry in registry.iter() {
            entry.install(&mut ecs);
        }
        ecs.component_hooks::<EntityGuid>()
            .on_insert(guid::on_insert)
            .on_replace(guid::on_replace);
        let settings = TimeSettings::from_cvars();
        ecs.insert_resource(Time::new());
        ecs.insert_resource(FixedTime::new(settings.fixed_step));
        ecs.insert_resource(settings);
        ecs.insert_resource(GuidIndex::new());

        let mut schedule = WorldSchedule::new();
        schedule.add_system(
            TickGroup::TransformPropagate,
            (propagate_transforms as fn(&mut marlin_ecs::World)).named("propagate_transforms"),
        );
        Self {
            ecs,
            kind,
            role: NetRole::Standalone,
            schedule,
            registry,
        }
    }

    pub fn kind(&self) -> WorldKind {
        self.kind
    }

    pub fn role(&self) -> NetRole {
        self.role
    }

    pub fn registry(&self) -> &Arc<ComponentRegistry> {
        &self.registry
    }

    pub fn ecs(&self) -> &marlin_ecs::World {
        &self.ecs
    }

    pub fn ecs_mut(&mut self) -> &mut marlin_ecs::World {
        &mut self.ecs
    }

    /// Adds a system to a tick group.
    pub fn add_system<M>(&mut self, group: TickGroup, system: impl IntoSystem<M>) -> &mut Self {
        self.schedule.add_system(group, system);
        self
    }

    pub fn schedule_mut(&mut self) -> &mut WorldSchedule {
        &mut self.schedule
    }

    pub fn time(&self) -> &Time {
        self.ecs.resource::<Time>().expect("worlds have a Time")
    }

    pub fn time_mut(&mut self) -> &mut Time {
        self.ecs.resource_mut::<Time>().expect("worlds have a Time")
    }

    pub fn fixed_time(&self) -> &FixedTime {
        self.ecs
            .resource::<FixedTime>()
            .expect("worlds have a FixedTime")
    }

    /// The live entity with `guid`, if any.
    pub fn entity_by_guid(&self, guid: EntityGuid) -> Option<Entity> {
        self.ecs.resource::<GuidIndex>().and_then(|i| i.get(guid))
    }

    /// Runs one frame `interval` seconds after the last (wall clock; [`Time`] clamps and scales
    /// it). Systems run on `jobs`.
    pub fn frame(&mut self, interval: f32, jobs: &JobSystem) {
        profile_scope!("World::frame");
        let settings = *self
            .ecs
            .resource::<TimeSettings>()
            .expect("worlds have settings");
        let time = self.time_mut();
        time.advance(interval, settings.max_delta);
        let (delta, paused) = (time.delta(), time.paused);
        let simulate = self.kind.simulates() && !paused;

        self.run(TickGroup::FrameStart, jobs);
        if simulate {
            self.run(TickGroup::PrePhysics, jobs);
            self.fixed_steps(delta, &settings, jobs);
            self.run(TickGroup::PostPhysics, jobs);
            self.run(TickGroup::LateUpdate, jobs);
        }
        self.run(TickGroup::TransformPropagate, jobs);
        self.run(TickGroup::Extract, jobs);
        self.run(TickGroup::FrameEnd, jobs);
        self.ecs.clear_trackers();
    }

    fn run(&mut self, group: TickGroup, jobs: &JobSystem) {
        self.schedule.run(group, &mut self.ecs, jobs);
    }

    /// The frame's fixed steps, recording [`Interpolated`] transforms around each.
    fn fixed_steps(&mut self, delta: f32, settings: &TimeSettings, jobs: &JobSystem) {
        // New interpolated entities start at rest where they are.
        let added: Vec<Entity> = self
            .ecs
            .query_ref_filtered::<Entity, Added<Interpolated>>()
            .collect();
        for e in added {
            let Some(&t) = self.ecs.get::<Transform>(e) else {
                continue;
            };
            if let Some(mut i) = self.ecs.get_mut::<Interpolated>(e) {
                i.previous = t;
                i.current = t;
            }
        }

        let steps = self
            .ecs
            .resource_mut::<FixedTime>()
            .expect("worlds have a FixedTime")
            .accumulate(delta, settings);
        for _ in 0..steps {
            profile_scope!("FixedStep");
            for (mut i,) in self.ecs.query::<(&mut Interpolated,)>() {
                // Not a change anyone needs to see: drawing reads it every frame.
                let i = i.bypass_change_detection();
                i.previous = i.current;
            }
            for group in TickGroup::FIXED {
                self.run(group, jobs);
            }
            for (mut i, t) in self.ecs.query::<(&mut Interpolated, &Transform)>() {
                i.bypass_change_detection().current = *t;
            }
            self.ecs
                .resource_mut::<FixedTime>()
                .expect("worlds have a FixedTime")
                .finish_step();
        }
        self.ecs
            .resource_mut::<FixedTime>()
            .expect("worlds have a FixedTime")
            .finish_frame();
    }
}
