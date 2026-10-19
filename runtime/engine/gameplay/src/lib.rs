//! The engine's basic gameplay components (0019 §12): what most games want without writing a
//! script, like Unreal's rotating movement component and spectator pawn.
//!
//! - [`RotatingMovement`]: turns an entity at a constant rate.
//! - [`SpectatorMovement`]: free flight driven by the frame's input; [`fly`] is its flight, which
//!   the editor's viewport camera shares.
//!
//! Both run in `PrePhysics`. Spectators read player 0's actions (0019 §9): the engine's spectator
//! context (`Marlin.Engine.Input.Spectator`, `content/engine/input`), which the module adds to
//! player 0 in every world when the content has it.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

use marlin_asset::Uuid;
use marlin_ecs::{Entity, IntoSystem, StorageType, With, Without};
use marlin_input_action::{ActionState, InputModule, Players};
use marlin_math::{DVec3, Quat, Vec3};
use marlin_reflect::Reflect;
use marlin_world::{
    ComponentFlags, EngineContext, Module, ModuleError, Registry, TickGroup, Time, Transform, World,
};

/// Turns the entity about `axis` at `speed`, from the rotation it had when the component was
/// added.
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(name = "marlin_gameplay::RotatingMovement", default)]
pub struct RotatingMovement {
    pub axis: Vec3,
    #[reflect(units = "°/s")]
    pub speed: f32,
}

impl Default for RotatingMovement {
    fn default() -> Self {
        Self {
            axis: Vec3::Y,
            speed: 45.0,
        }
    }
}

/// The rotation a [`RotatingMovement`] turns from (runtime).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RotatingBase(pub Quat);

/// Free flight: look with the right mouse button held (or the right stick), move with WASD/QE
/// (or the left stick and triggers), Shift to go faster.
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(name = "marlin_gameplay::SpectatorMovement", default)]
pub struct SpectatorMovement {
    #[reflect(units = "m/s", range = 0.0..)]
    pub speed: f32,
}

impl Default for SpectatorMovement {
    fn default() -> Self {
        Self { speed: 10.0 }
    }
}

/// Where a [`SpectatorMovement`] looks, in radians (runtime): yaw about +Y, then pitch.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SpectatorLook {
    pub yaw: f32,
    pub pitch: f32,
}

/// `PrePhysics`: turns rotating entities, from the rotation each had when it started.
pub fn rotate(world: &mut marlin_ecs::World) {
    let new: Vec<(Entity, Quat)> = world
        .query_ref_filtered::<(Entity, &Transform), (With<RotatingMovement>, Without<RotatingBase>)>()
        .map(|(e, t)| (e, t.rotation))
        .collect();
    for (e, rotation) in new {
        world.insert(e, RotatingBase(rotation));
    }
    let time = world.resource::<Time>().map_or(0.0, Time::elapsed) as f32;
    for (mut transform, spin, base) in
        world.query::<(&mut Transform, &RotatingMovement, &RotatingBase)>()
    {
        let axis = spin.axis.normalize_or_zero();
        let axis = if axis == Vec3::ZERO { Vec3::Y } else { axis };
        transform.rotation = base.0 * Quat::from_axis_angle(axis, spin.speed.to_radians() * time);
    }
}

/// One frame of flight input, read from the spectator's actions.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct FlyInput {
    /// Turn, in radians: yaw (right is positive), pitch (up is positive).
    pub look: [f32; 2],
    /// Right and forward, each in [-1, 1] or so.
    pub movement: [f32; 2],
    pub rise: f32,
    pub boost: bool,
}

impl FlyInput {
    /// The actions' states this frame, through `action` (a player's or another input user's).
    pub fn read(actions: &SpectatorActions, action: impl Fn(Uuid) -> ActionState) -> Self {
        Self {
            look: action(actions.look).axis2(),
            movement: action(actions.movement).axis2(),
            rise: action(actions.rise).axis1(),
            boost: action(actions.boost).triggered,
        }
    }
}

/// Where something looking along `rotation` looks: yaw and pitch (roll is dropped).
pub fn look_of(rotation: Quat) -> SpectatorLook {
    let f = rotation.mul_vec3(Vec3::FORWARD);
    SpectatorLook {
        yaw: marlin_math::scalar::atan2(-f.x, -f.z),
        pitch: marlin_math::scalar::asin(f.y.clamp(-1.0, 1.0)),
    }
}

/// Free flight: turns `look` by `input` and moves `translation` along it at `speed` (three times
/// it boosted) for `dt` seconds; returns the rotation `look` is now. What spectators and the
/// editor's viewport camera do.
pub fn fly(
    look: &mut SpectatorLook,
    translation: &mut DVec3,
    input: &FlyInput,
    speed: f32,
    dt: f32,
) -> Quat {
    look.yaw -= input.look[0];
    look.pitch = (look.pitch + input.look[1]).clamp(-1.5, 1.5);
    let rotation = Quat::from_euler_yxz(look.yaw, look.pitch, 0.0);
    let [right, forward] = input.movement;
    let movement = rotation.mul_vec3(Vec3::new(right, 0.0, -forward)) + Vec3::Y * input.rise;
    let boost = if input.boost { 3.0 } else { 1.0 };
    *translation += movement * (speed * boost * dt);
    rotation
}

/// `PrePhysics`: flies spectators with player 0's actions.
pub fn spectate(world: &mut marlin_ecs::World) {
    let new: Vec<(Entity, Quat)> = world
        .query_ref_filtered::<(Entity, &Transform), (With<SpectatorMovement>, Without<SpectatorLook>)>()
        .map(|(e, t)| (e, t.rotation))
        .collect();
    for (e, rotation) in new {
        world.insert(e, look_of(rotation));
    }
    let (Some(&actions), Some(players)) = (
        world.resource::<SpectatorActions>(),
        world.resource::<Players>(),
    ) else {
        return;
    };
    let Some(player) = players.player(0) else {
        return;
    };
    let input = FlyInput::read(&actions, |a| player.action(a));
    if input == FlyInput::default() {
        return;
    }
    let dt = world.resource::<Time>().map_or(0.0, Time::delta);
    for (mut transform, mut look, spectator) in
        world.query::<(&mut Transform, &mut SpectatorLook, &SpectatorMovement)>()
    {
        let transform = &mut *transform;
        transform.rotation = fly(
            &mut look,
            &mut transform.translation,
            &input,
            spectator.speed,
            dt,
        );
    }
}

/// The spectator's context and actions, by asset (a world resource; an engine resource of the
/// gameplay module once started, when the content has them).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SpectatorActions {
    pub context: Uuid,
    pub movement: Uuid,
    pub look: Uuid,
    pub rise: Uuid,
    pub boost: Uuid,
}

impl SpectatorActions {
    /// The spectator's mapping context.
    pub const CONTEXT: &'static str = "Marlin.Engine.Input.Spectator";

    /// Finds them by name in `vfs`.
    pub fn find(vfs: &marlin_asset::Vfs) -> Option<Self> {
        Some(Self {
            context: vfs.named(Self::CONTEXT)?,
            movement: vfs.named("Marlin.Engine.Input.Move")?,
            look: vfs.named("Marlin.Engine.Input.Look")?,
            rise: vfs.named("Marlin.Engine.Input.Rise")?,
            boost: vfs.named("Marlin.Engine.Input.Boost")?,
        })
    }
}

/// The gameplay module: its components and their systems.
#[derive(Default)]
pub struct GameplayModule {
    spectator: Option<SpectatorActions>,
}

impl GameplayModule {
    pub const NAME: &'static str = "Gameplay";

    pub fn new() -> Self {
        Self::default()
    }
}

impl Module for GameplayModule {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn dependencies(&self) -> &'static [&'static str] {
        &[marlin_world::WorldModule::NAME, InputModule::NAME]
    }

    fn register(&self, r: &mut Registry) {
        r.components()
            .add::<RotatingMovement>(ComponentFlags::SAVED)
            .add::<SpectatorMovement>(ComponentFlags::SAVED)
            .add_runtime::<RotatingBase>(StorageType::Table)
            .add_runtime::<SpectatorLook>(StorageType::Table);
        r.add_system(TickGroup::PrePhysics, || {
            (rotate as fn(&mut marlin_ecs::World)).named("rotate")
        })
        .add_system(TickGroup::PrePhysics, || {
            (spectate as fn(&mut marlin_ecs::World)).named("spectate")
        });
    }

    fn startup(&mut self, cx: &mut EngineContext<'_, '_>) -> Result<(), ModuleError> {
        self.spectator = SpectatorActions::find(cx.vfs);
        if self.spectator.is_none() {
            marlin_diag::warn!(
                "no `{}` in the mounted content: spectators do not move",
                SpectatorActions::CONTEXT
            );
        }
        Ok(())
    }

    fn init_world(&mut self, world: &mut World, _: &mut EngineContext<'_, '_>) {
        let Some(actions) = self.spectator else {
            return;
        };
        world.ecs_mut().insert_resource(actions);
        if let Some(p) = world
            .ecs_mut()
            .resource_mut::<Players>()
            .and_then(|p| p.player_mut(0))
        {
            p.add_context(actions.context, 0);
        }
    }
}
