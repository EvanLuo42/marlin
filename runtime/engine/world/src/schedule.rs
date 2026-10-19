//! The tick groups a world's frame runs in, and the schedule holding each group's systems
//! (0019 §4.2).

use marlin_ecs::{IntoSystem, Schedule};
use marlin_jobs::JobSystem;

use crate::WorldKind;

/// A phase of the frame. Groups run one after another in [`TickGroup::ALL`] order; inside a
/// group, systems run in parallel where their declared access allows (`marlin-ecs`'s waves) and
/// in the order `before`/`after` give. The three `Fixed*`/`Physics` groups run once per fixed
/// step, zero or more times a frame ([`FixedTime`](crate::FixedTime)).
///
/// The list belongs to the world: modules put systems in groups but do not add groups.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum TickGroup {
    /// Time, input actions, asset callbacks, sublevel loads and unloads.
    FrameStart,
    /// Most gameplay: script `Tick` by default.
    PrePhysics,
    /// Gameplay that must see every fixed step (script `FixedTick`, forces).
    FixedPrePhysics,
    /// The physics step.
    Physics,
    /// Reactions to the step (contacts, triggers).
    FixedPostPhysics,
    /// Gameplay that reads the frame's physics results; animation.
    PostPhysics,
    /// Cameras and things that follow others.
    LateUpdate,
    /// Computes `GlobalTransform`s ([`propagate_transforms`](crate::propagate_transforms)).
    TransformPropagate,
    /// Subsystems copy what changed into their snapshots (render's `RenderDelta`, audio).
    Extract,
    /// Cleanup before the world's change trackers are cleared.
    FrameEnd,
}

impl TickGroup {
    pub const COUNT: usize = 10;

    pub const ALL: [Self; Self::COUNT] = [
        Self::FrameStart,
        Self::PrePhysics,
        Self::FixedPrePhysics,
        Self::Physics,
        Self::FixedPostPhysics,
        Self::PostPhysics,
        Self::LateUpdate,
        Self::TransformPropagate,
        Self::Extract,
        Self::FrameEnd,
    ];

    pub const FIXED: [Self; 3] = [Self::FixedPrePhysics, Self::Physics, Self::FixedPostPhysics];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::FrameStart => "FrameStart",
            Self::PrePhysics => "PrePhysics",
            Self::FixedPrePhysics => "FixedPrePhysics",
            Self::Physics => "Physics",
            Self::FixedPostPhysics => "FixedPostPhysics",
            Self::PostPhysics => "PostPhysics",
            Self::LateUpdate => "LateUpdate",
            Self::TransformPropagate => "TransformPropagate",
            Self::Extract => "Extract",
            Self::FrameEnd => "FrameEnd",
        }
    }

    /// Runs once per fixed step rather than once per frame.
    pub const fn is_fixed(self) -> bool {
        matches!(
            self,
            Self::FixedPrePhysics | Self::Physics | Self::FixedPostPhysics
        )
    }

    /// Simulates the game: skipped by editor worlds and paused worlds, which still place, draw
    /// and clean up.
    pub const fn is_gameplay(self) -> bool {
        !matches!(
            self,
            Self::FrameStart | Self::TransformPropagate | Self::Extract | Self::FrameEnd
        )
    }

    /// Whether a world of `kind` runs this group (when not paused).
    pub const fn runs_in(self, kind: WorldKind) -> bool {
        kind.simulates() || !self.is_gameplay()
    }
}

/// One `marlin-ecs` [`Schedule`] per [`TickGroup`].
#[derive(Default)]
pub struct WorldSchedule {
    groups: [Schedule; TickGroup::COUNT],
}

impl WorldSchedule {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a system to `group`.
    pub fn add_system<M>(&mut self, group: TickGroup, system: impl IntoSystem<M>) -> &mut Self {
        self.groups[group.index()].add_system(system);
        self
    }

    /// The schedule of one group, to configure its sets.
    pub fn group_mut(&mut self, group: TickGroup) -> &mut Schedule {
        &mut self.groups[group.index()]
    }

    /// Runs one group's systems once.
    pub fn run(&mut self, group: TickGroup, world: &mut marlin_ecs::World, jobs: &JobSystem) {
        self.groups[group.index()].run(world, jobs);
    }
}
