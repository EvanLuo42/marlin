//! The simulation side of a running game: assets, the world, the entry level and the engine's
//! systems. Each [`Engine::frame`] advances it and returns the frame's [`RenderDelta`].

use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_asset::{AssetServer, BlobStreamer, Handle, LoadState, Uuid, Vfs};
use marlin_containers::{HashMap, HashMapExt};
use marlin_ecs::{Changed, Entity, With, World};
use marlin_input::{InputState, Key, MouseButton};
use marlin_io::IoQueue;
use marlin_jobs::JobSystem;
use marlin_material::DefaultMaterial;
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::scene::{
    InstanceDesc, InstanceId, InstanceUpdate, LightDesc, LightId, LightKind, LightUpdate,
    MaterialId, MaterialUpdate, MeshId, MeshUpdate,
};
use marlin_render::{Camera, SceneIds, SceneUpdates};

use crate::delta::{MeshUpload, RenderDelta};
use crate::level::{Component, DEFAULT_LIGHT_RANGE, DEFAULT_SPOT_ANGLE, Level};
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

/// A point or spot light at the entity's transform (the simulation side of a light), in the
/// renderer's units.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Light {
    pub kind: LightKind,
    /// Luminous intensity in candela times the linear color.
    pub intensity: [f32; 3],
    /// The emitting sphere's radius, in meters.
    pub radius: f32,
    /// Where its light fades out, in meters.
    pub range: f32,
    /// A spot light's cone: cosines of the inner and outer half angles.
    pub cone: [f32; 2],
}

impl Light {
    /// A level's point or spot light component: lumens to candela (Φ / 4π, a spot's as a point
    /// light's), angles to cosines, zeros to their defaults.
    pub fn from_component(component: &Component) -> Option<Self> {
        let (kind, color, lumens, radius, range, angles) = match *component {
            Component::PointLight {
                color,
                intensity,
                radius,
                range,
            } => (LightKind::Point, color, intensity, radius, range, [0.0; 2]),
            Component::SpotLight {
                color,
                intensity,
                radius,
                range,
                inner_angle,
                outer_angle,
            } => (
                LightKind::Spot,
                color,
                intensity,
                radius,
                range,
                [inner_angle, outer_angle],
            ),
            _ => return None,
        };
        let color = if color == [0.0; 3] { [1.0; 3] } else { color };
        let candela = lumens.max(0.0) / (4.0 * core::f32::consts::PI);
        let outer = if angles[1] > 0.0 {
            angles[1]
        } else {
            DEFAULT_SPOT_ANGLE
        };
        let inner = angles[0].clamp(0.0, outer);
        Some(Self {
            kind,
            intensity: color.map(|c| c * candela),
            radius: radius.max(0.0),
            range: if range > 0.0 {
                range
            } else {
                DEFAULT_LIGHT_RANGE
            },
            cone: [
                marlin_math::scalar::cos(inner.to_radians()),
                marlin_math::scalar::cos(outer.to_radians()),
            ],
        })
    }

    /// The render scene's description of it at `transform`: a spot light shines down -Z.
    pub fn desc(&self, transform: &Transform) -> LightDesc {
        let p = transform.translation;
        let d = transform.rotation.mul_vec3(-Vec3::Z).normalize_or_zero();
        LightDesc {
            kind: self.kind,
            radiance: self.intensity,
            radius: self.radius,
            position: [p.x, p.y, p.z],
            direction: [d.x, d.y, d.z],
            range: self.range,
            cone: self.cone,
        }
    }
}

/// Marks entities a level spawned, so a reload of the level replaces them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FromLevel;

pub struct Engine<'v> {
    vfs: &'v Vfs,
    io: IoQueue<'v>,
    pub assets: AssetServer,
    /// Reads the streamed blobs (mesh pages) the render side asks for, on its own queue.
    pub streamer: Arc<BlobStreamer>,
    stream_io: IoQueue<'v>,
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
    /// Entities lit, with their row in the render scene's light table.
    lights: HashMap<Entity, LightId>,
    /// What static meshes draw with until they name materials.
    default_material: MaterialId,
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
        let ids = SceneIds::new();
        let default_material = ids.materials.reserve();
        let mut updates = SceneUpdates::default();
        updates.materials.push(
            default_material,
            MaterialUpdate::Add(DefaultMaterial::default().desc()),
        );
        let streamer = Arc::new(BlobStreamer::new());
        Ok(Self {
            vfs,
            io: IoQueue::new(64)?,
            assets: AssetServer::new(engine_loaders(&streamer)),
            streamer,
            stream_io: IoQueue::new(64)?,
            world: World::new(),
            level: None,
            spawned: None,
            ids,
            default_material,
            meshes: HashMap::new(),
            instances: HashMap::new(),
            lights: HashMap::new(),
            updates,
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
        self.streamer.update(self.vfs, &mut self.stream_io);

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
                            ..Camera::default()
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
                    Component::PointLight { .. } | Component::SpotLight { .. } => {
                        if let Some(light) = Light::from_component(c) {
                            self.world.insert(entity, light);
                        }
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

    /// The lights' changes since the last delta, into `updates`: entities that lost their
    /// [`Light`] (or are gone) leave the light table; new ones, and those whose light or
    /// transform changed, are sent whole.
    fn extract_lights(&mut self, updates: &mut SceneUpdates) {
        let world = &self.world;
        let gone: Vec<Entity> = world
            .removed::<Light>()
            .filter(|&e| !world.has::<Light>(e))
            .collect();
        for e in gone {
            if let Some(id) = self.lights.remove(&e) {
                updates.lights.push(id, LightUpdate::Remove);
            }
        }
        let mut changed: Vec<Entity> = world
            .query_ref_filtered::<(Entity, &Light), Changed<Light>>()
            .map(|(e, _)| e)
            .chain(
                world
                    .query_ref_filtered::<(Entity, &Transform), (Changed<Transform>, With<Light>)>()
                    .map(|(e, _)| e),
            )
            .collect();
        changed.sort_unstable();
        changed.dedup();
        for e in changed {
            let (Some(light), Some(transform)) = (world.get::<Light>(e), world.get::<Transform>(e))
            else {
                continue;
            };
            let desc = light.desc(transform);
            match self.lights.get(&e) {
                Some(&id) => updates.lights.push(id, LightUpdate::Set(desc)),
                None => {
                    let id = self.ids.lights.reserve();
                    updates.lights.push(id, LightUpdate::Add(desc));
                    self.lights.insert(e, id);
                }
            }
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
                            material: self.default_material,
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
        self.extract_lights(&mut updates);
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
