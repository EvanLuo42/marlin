//! The engine's basic gameplay components (0019 §12): what most games want without writing a
//! script, like Unreal's rotating movement component and spectator pawn.
//!
//! - [`RotatingMovement`]: turns an entity at a constant rate.
//! - [`SpectatorMovement`]: free flight driven by the frame's input.
//!
//! Both run in `PrePhysics`. The input is the world's [`InputState`] resource, which the host
//! sets each frame (none: spectators stand still). Input actions replace it (0019 §9).
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

use marlin_ecs::{Entity, IntoSystem, StorageType, With, Without};
use marlin_input::{InputState, Key, MouseButton};
use marlin_math::{Quat, Vec3};
use marlin_reflect::Reflect;
use marlin_world::{ComponentFlags, Module, Registry, TickGroup, Time, Transform};

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
    let Some(&state) = world.resource::<InputState>() else {
        return;
    };
    let dt = world.resource::<Time>().map_or(0.0, Time::delta);
    let pad = state.gamepad(0);
    let mut look = pad.right_stick().map(|v| v * 2.5 * dt);
    if state.mouse().down(MouseButton::Right) {
        let [dx, dy] = state.mouse().delta();
        look[0] += dx * 0.003;
        look[1] -= dy * 0.003;
    }
    let key = |k| if state.key_down(k) { 1.0 } else { 0.0 };
    let [stick_x, stick_y] = pad.left_stick();
    let right = key(Key::D) - key(Key::A) + stick_x;
    let forward = key(Key::W) - key(Key::S) + stick_y;
    let up = key(Key::E) - key(Key::Q) + pad.right_trigger() - pad.left_trigger();
    let boost = if state.shift() { 3.0 } else { 1.0 };
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

/// The gameplay module: its components and their systems.
pub struct GameplayModule;

impl GameplayModule {
    pub const NAME: &'static str = "Gameplay";
}

impl Module for GameplayModule {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn dependencies(&self) -> &'static [&'static str] {
        &[marlin_world::WorldModule::NAME]
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
}
