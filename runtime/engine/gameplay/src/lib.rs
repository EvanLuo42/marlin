//! The engine's basic gameplay components (0019 §12): what most games want without writing a
//! script, like Unreal's rotating movement component and spectator pawn.
//!
//! - [`RotatingMovement`]: turns an entity at a constant rate.
//! - [`SpectatorMovement`]: free flight driven by the frame's input.
//!
//! Both run in `PrePhysics`. Spectators read player 0's actions (0019 §9): the engine's spectator
//! context (`Marlin.Engine.Input.Spectator`, `content/engine/input`), which the module adds to
//! player 0 in every world when the content has it.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

use marlin_asset::Uuid;
use marlin_ecs::{Entity, IntoSystem, StorageType, With, Without};
use marlin_input_action::{InputModule, Players};
use marlin_math::{Quat, Vec3};
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

/// `PrePhysics`: flies spectators with the frame's input.
pub fn spectate(world: &mut marlin_ecs::World) {
    let new: Vec<(Entity, Quat)> = world
        .query_ref_filtered::<(Entity, &Transform), (With<SpectatorMovement>, Without<SpectatorLook>)>()
        .map(|(e, t)| (e, t.rotation))
        .collect();
    for (e, rotation) in new {
        // The yaw and pitch of where it looks (roll is dropped).
        let f = rotation.mul_vec3(Vec3::FORWARD);
        let look = SpectatorLook {
            yaw: marlin_math::scalar::atan2(-f.x, -f.z),
            pitch: marlin_math::scalar::asin(f.y.clamp(-1.0, 1.0)),
        };
        world.insert(e, look);
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
    let look = player.action(actions.look).axis2();
    let [right, forward] = player.action(actions.movement).axis2();
    let up = player.action(actions.rise).axis1();
    let boost = if player.action(actions.boost).triggered {
        3.0
    } else {
        1.0
    };
    let dt = world.resource::<Time>().map_or(0.0, Time::delta);
    for (mut transform, mut fly, spectator) in
        world.query::<(&mut Transform, &mut SpectatorLook, &SpectatorMovement)>()
    {
        if look != [0.0; 2] {
            fly.yaw -= look[0];
            fly.pitch = (fly.pitch + look[1]).clamp(-1.5, 1.5);
        }
        let rotation = Quat::from_euler_yxz(fly.yaw, fly.pitch, 0.0);
        let movement = rotation.mul_vec3(Vec3::new(right, 0.0, -forward)) + Vec3::Y * up;
        if look != [0.0; 2] || movement != Vec3::ZERO {
            transform.rotation = rotation;
            transform.translation += movement * (spectator.speed * boost * dt);
        }
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
