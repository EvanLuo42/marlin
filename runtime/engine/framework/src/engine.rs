//! The simulation side of a running game: assets, the world, the entry level and the engine's
//! systems. Each [`Engine::frame`] advances it and returns the frame's [`RenderDelta`].

use alloc::vec::Vec;

use marlin_asset::{AssetServer, Handle, LoadState, Uuid, Vfs};
use marlin_containers::{HashMap, HashMapExt};
use marlin_ecs::{Changed, Entity, With, World};
use marlin_input::{InputState, Key, MouseButton};
use marlin_io::IoQueue;
use marlin_jobs::JobSystem;
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::scene::{InstanceDesc, InstanceId, InstanceUpdate, MeshId, MeshUpdate};
use marlin_render::{Camera, SceneIds, SceneUpdates};

use crate::delta::{MeshUpload, RenderDelta};
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
    /// Where the scene's rows come from: the render side's scene shares them.
    ids: SceneIds,
    /// Meshes the level uses: their scene row and the version last sent to the render side.
    meshes: HashMap<Uuid, MeshEntry>,
    /// Entities drawn, with what the render scene has of them.
    instances: HashMap<Entity, InstanceEntry>,
    /// Changes made outside extract (spawning a level), sent with the next delta.
    updates: SceneUpdates,
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
            ids: SceneIds::new(),
            meshes: HashMap::new(),
            instances: HashMap::new(),
            updates: SceneUpdates::default(),
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
            && self.meshes.values().all(|m| {
                m.sent.is_some() || !matches!(self.assets.state(&m.handle), LoadState::Loading)
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
        let mut delta = self.extract();
        delta.dt = dt;
        // The frame's changes have been extracted: start the next change-detection period.
        self.world.clear_trackers();
        delta
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
        self.updates.environment = Some(level.environment.to_render());
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
            if let Some(entry) = self.meshes.remove(&m) {
                self.updates.meshes.push(entry.id, MeshUpdate::Remove);
            }
        }
        for m in used {
            if !self.meshes.contains_key(&m) {
                let id = self.ids.meshes.reserve();
                self.updates.meshes.push(id, MeshUpdate::Add);
                let handle = self.assets.load(m);
                self.meshes.insert(
                    m,
                    MeshEntry {
                        handle,
                        id,
                        sent: None,
                    },
                );
            }
        }
    }

    fn spin(&mut self) {
        let time = self.time;
        for (mut transform, spin) in self.world.query::<(&mut Transform, &Spin)>() {
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
        for (mut transform, mut fly) in self.world.query::<(&mut Transform, &mut FlyCamera)>() {
            fly.yaw -= look[0];
            fly.pitch = (fly.pitch + look[1]).clamp(-1.5, 1.5);
            let rotation = Quat::from_euler_yxz(fly.yaw, fly.pitch, 0.0);
            let movement = rotation.mul_vec3(Vec3::new(right, 0.0, -forward)) + Vec3::Y * up;
            transform.rotation = rotation;
            transform.translation += movement * (fly.speed * boost * dt);
        }
    }

    /// The render delta: changes since the last one (0009 §1.2). Instances are entities with a
    /// [`Transform`] and a [`StaticMesh`]; only those added, changed or removed since the last
    /// [`World::clear_trackers`] produce updates, and a moved entity sends just its transform.
    fn extract(&mut self) -> RenderDelta {
        let mut updates = core::mem::take(&mut self.updates);
        let mut uploads = Vec::new();
        for entry in self.meshes.values_mut() {
            if let Some(loaded) = self.assets.get(&entry.handle) {
                let version = self.assets.version(&entry.handle);
                if entry.sent != Some(version) {
                    uploads.push(MeshUpload {
                        mesh: entry.id,
                        version,
                        cooked: loaded.clone(),
                    });
                    entry.sent = Some(version);
                }
            }
        }
        uploads.sort_unstable_by_key(|u| u.mesh.index());

        // Entities that lost their mesh, or are gone.
        let world = &self.world;
        let gone: Vec<Entity> = world
            .removed::<StaticMesh>()
            .filter(|&e| !world.has::<StaticMesh>(e))
            .collect();
        for e in gone {
            if let Some(entry) = self.instances.remove(&e) {
                updates.instances.push(entry.id, InstanceUpdate::Remove);
            }
        }
        // New entities, and changed meshes or colors.
        let frame = self.frame;
        for (e, transform, m) in self
            .world
            .query_ref_filtered::<(Entity, &Transform, &StaticMesh), Changed<StaticMesh>>()
        {
            // A mesh the level did not list (game code spawned it): load it now.
            let mesh = match self.meshes.get(&m.mesh) {
                Some(entry) => entry.id,
                None => {
                    let id = self.ids.meshes.reserve();
                    updates.meshes.push(id, MeshUpdate::Add);
                    let handle = self.assets.load(m.mesh);
                    self.meshes.insert(
                        m.mesh,
                        MeshEntry {
                            handle,
                            id,
                            sent: None,
                        },
                    );
                    id
                }
            };
            match self.instances.get_mut(&e) {
                None => {
                    let id = self.ids.instances.reserve();
                    updates.instances.push(
                        id,
                        InstanceUpdate::Add(InstanceDesc {
                            mesh,
                            transform: *transform,
                            color: m.color,
                            flags: marlin_render::scene::flags::SHADOW,
                        }),
                    );
                    self.instances.insert(
                        e,
                        InstanceEntry {
                            id,
                            mesh,
                            color: m.color,
                            added: frame,
                        },
                    );
                }
                Some(entry) => {
                    if entry.mesh != mesh {
                        entry.mesh = mesh;
                        updates.instances.push(entry.id, InstanceUpdate::Mesh(mesh));
                    }
                    if entry.color != m.color {
                        entry.color = m.color;
                        updates
                            .instances
                            .push(entry.id, InstanceUpdate::Color(m.color));
                    }
                }
            }
        }
        // Moved entities (new ones were added where they are).
        for (e, transform) in self
            .world
            .query_ref_filtered::<(Entity, &Transform), (Changed<Transform>, With<StaticMesh>)>()
        {
            if let Some(entry) = self.instances.get(&e)
                && entry.added != frame
            {
                updates
                    .instances
                    .push(entry.id, InstanceUpdate::Transform(*transform));
            }
        }
        RenderDelta {
            frame: self.frame,
            dt: 0.0,
            updates,
            uploads,
            camera: self
                .world
                .query_ref::<(&Transform, &Camera)>()
                .next()
                .map(|(t, c)| (*t, *c)),
        }
    }

    /// The allocators the render scene must take its rows from: give them to
    /// [`RenderSide::new`](crate::RenderSide::new).
    pub fn scene_ids(&self) -> SceneIds {
        self.ids.clone()
    }

    /// Instances the render scene has, as of the last delta.
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }
}

/// A mesh the level uses.
struct MeshEntry {
    handle: Handle<LoadedMesh>,
    /// Its row in the render scene.
    id: MeshId,
    /// The version last sent to the render side.
    sent: Option<u32>,
}

/// What the render scene has of a drawn entity.
struct InstanceEntry {
    id: InstanceId,
    mesh: MeshId,
    color: [f32; 4],
    /// The frame it was added in.
    added: u64,
}
