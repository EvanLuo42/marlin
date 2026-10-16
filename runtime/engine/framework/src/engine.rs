//! The simulation side of a running game: assets, the world, the entry level and the engine's
//! systems. Each [`Engine::frame`] advances it and returns the frame's [`RenderDelta`].

use alloc::vec::Vec;

use marlin_asset::{AssetServer, Handle, LoadState, Uuid, Vfs};
use marlin_containers::{HashMap, HashMapExt};
use marlin_ecs::{Entity, World};
use marlin_input::{InputState, Key, MouseButton};
use marlin_io::IoQueue;
use marlin_jobs::JobSystem;
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::{Camera, Environment};

use crate::delta::{MeshUpload, RenderDelta, RenderInstance};
use crate::level::{Component, Level};
use crate::loaders::{LoadedMesh, engine_loaders};

/// Draws a mesh asset at the entity's transform (the simulation side of a static mesh).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct StaticMesh {
    pub mesh: Uuid,
    pub color: [f32; 4],
}

/// Turns the entity about `axis`, `speed` radians per second, from its authored rotation.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Spin {
    pub axis: Vec3,
    pub speed: f32,
    pub base: Quat,
}

/// Look and move with mouse and keyboard or a gamepad.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FlyCamera {
    pub speed: f32,
    pub yaw: f32,
    pub pitch: f32,
}

/// Marks entities a level spawned, so a reload of the level replaces them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FromLevel;

pub struct Engine<'v> {
    vfs: &'v Vfs,
    io: IoQueue<'v>,
    pub assets: AssetServer,
    pub world: World,
    level: Option<Handle<Level>>,
    /// The level version whose entities are in the world.
    spawned: Option<u32>,
    /// Meshes the level uses, and the version last sent to the render side.
    meshes: HashMap<Uuid, (Handle<LoadedMesh>, Option<u32>)>,
    released: Vec<Uuid>,
    environment: Environment,
    frame: u64,
    time: f32,
}

#[derive(Debug, marlin_error::Error)]
#[error("asset IO")]
pub struct EngineError(#[from] pub marlin_io::Error);

impl<'v> Engine<'v> {
    /// An engine loading from `vfs`, with the engine's loaders.
    pub fn new(vfs: &'v Vfs) -> Result<Self, EngineError> {
        Ok(Self {
            vfs,
            io: IoQueue::new(64)?,
            assets: AssetServer::new(engine_loaders()),
            world: World::new(),
            level: None,
            spawned: None,
            meshes: HashMap::new(),
            released: Vec::new(),
            environment: Environment::default(),
            frame: 0,
            time: 0.0,
        })
    }

    /// Starts loading the level `uuid`; its entities replace the current level's once loaded.
    pub fn load_level(&mut self, uuid: Uuid) {
        self.level = Some(self.assets.load(uuid));
        self.spawned = None;
    }

    pub fn level_state(&self) -> LoadState {
        match &self.level {
            Some(h) => self.assets.state(h),
            None => LoadState::Failed(marlin_asset::AssetError::NotFound),
        }
    }

    /// Whether the level and every mesh it uses have loaded and been handed to the renderer.
    pub fn is_ready(&self) -> bool {
        self.spawned.is_some()
            && self.meshes.values().all(|(h, sent)| {
                sent.is_some() || !matches!(self.assets.state(h), LoadState::Loading)
            })
    }

    pub fn frame_count(&self) -> u64 {
        self.frame
    }

    /// One frame: loads, (re)spawns the level, runs the systems and takes the render delta.
    /// `input` drives fly cameras; `jobs` decompresses loads in parallel.
    pub fn frame(
        &mut self,
        dt: f32,
        input: Option<&InputState>,
        jobs: Option<&JobSystem>,
    ) -> RenderDelta {
        self.frame += 1;
        self.time += dt;
        self.assets.update(self.vfs, &mut self.io, jobs);

        // The level: spawn once loaded, again when it reloads.
        if let Some(level) = &self.level {
            let version = self.assets.version(level);
            if self.spawned != Some(version)
                && let Some(l) = self.assets.get(level)
            {
                let l = l.clone();
                self.spawn(&l);
                self.spawned = Some(version);
            }
        }

        self.spin();
        if let Some(input) = input {
            self.fly(input, dt);
        }
        self.extract()
    }

    fn spawn(&mut self, level: &Level) {
        let old: Vec<Entity> = self
            .world
            .query_ref::<(Entity, &FromLevel)>()
            .map(|(e, _)| e)
            .collect();
        for e in old {
            self.world.despawn(e);
        }
        self.environment = level.environment.to_render();
        let mut used: Vec<Uuid> = Vec::new();
        for desc in &level.entities {
            let transform = desc.transform.to_transform();
            let entity = self.world.spawn((transform, FromLevel));
            for c in &desc.components {
                match *c {
                    Component::StaticMesh { mesh, color } => {
                        used.push(mesh.target);
                        self.world.insert(
                            entity,
                            StaticMesh {
                                mesh: mesh.target,
                                color,
                            },
                        );
                    }
                    Component::Camera { fov_y, near } => {
                        let camera = Camera {
                            fov_y: if fov_y > 0.0 {
                                fov_y.to_radians()
                            } else {
                                Camera::default().fov_y
                            },
                            near: if near > 0.0 {
                                near
                            } else {
                                Camera::default().near
                            },
                        };
                        self.world.insert(entity, camera);
                    }
                    Component::Spin { axis, speed } => {
                        let axis = Vec3::new(axis[0], axis[1], axis[2]).normalize_or_zero();
                        self.world.insert(
                            entity,
                            Spin {
                                axis: if axis == Vec3::ZERO { Vec3::Y } else { axis },
                                speed: speed.to_radians(),
                                base: transform.rotation,
                            },
                        );
                    }
                    Component::FlyCamera { speed } => {
                        let [yaw, pitch, _] = desc.transform.rotation.map(f32::to_radians);
                        self.world.insert(
                            entity,
                            FlyCamera {
                                speed: if speed > 0.0 { speed } else { 10.0 },
                                yaw,
                                pitch,
                            },
                        );
                    }
                }
            }
        }
        // Keep exactly the meshes the level uses.
        used.sort_unstable();
        used.dedup();
        let gone: Vec<Uuid> = self
            .meshes
            .keys()
            .copied()
            .filter(|m| used.binary_search(m).is_err())
            .collect();
        for m in gone {
            self.meshes.remove(&m);
            self.released.push(m);
        }
        for m in used {
            if !self.meshes.contains_key(&m) {
                let handle = self.assets.load(m);
                self.meshes.insert(m, (handle, None));
            }
        }
    }

    fn spin(&mut self) {
        let time = self.time;
        for (transform, spin) in self.world.query::<(&mut Transform, &Spin)>() {
            transform.rotation = spin.base * Quat::from_axis_angle(spin.axis, spin.speed * time);
        }
    }

    fn fly(&mut self, state: &InputState, dt: f32) {
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
        for (transform, fly) in self.world.query::<(&mut Transform, &mut FlyCamera)>() {
            fly.yaw -= look[0];
            fly.pitch = (fly.pitch + look[1]).clamp(-1.5, 1.5);
            let rotation = Quat::from_euler_yxz(fly.yaw, fly.pitch, 0.0);
            let movement = rotation.mul_vec3(Vec3::new(right, 0.0, -forward)) + Vec3::Y * up;
            transform.rotation = rotation;
            transform.translation += movement * (fly.speed * boost * dt);
        }
    }

    fn extract(&mut self) -> RenderDelta {
        let mut uploads = Vec::new();
        for (&mesh, (handle, sent)) in &mut self.meshes {
            if let Some(loaded) = self.assets.get(handle) {
                let version = self.assets.version(handle);
                if *sent != Some(version) {
                    uploads.push(MeshUpload {
                        mesh,
                        version,
                        geometry: loaded.0.clone(),
                    });
                    *sent = Some(version);
                }
            }
        }
        uploads.sort_unstable_by_key(|u| u.mesh);
        RenderDelta {
            frame: self.frame,
            uploads,
            releases: core::mem::take(&mut self.released),
            camera: self
                .world
                .query_ref::<(&Transform, &Camera)>()
                .next()
                .map(|(t, c)| (*t, *c)),
            environment: self.environment,
            instances: self
                .world
                .query_ref::<(&Transform, &StaticMesh)>()
                .map(|(t, m)| RenderInstance {
                    mesh: m.mesh,
                    transform: *t,
                    color: m.color,
                })
                .collect(),
        }
    }
}
