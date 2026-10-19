//! The simulation side of a running game: assets, the world, the entry level and the engine's
//! systems. Each [`Engine::frame`] advances it and returns the frame's [`RenderDelta`].
//!
//! The world runs the systems in its tick groups (0019 §4.2): [`RotatingMovement`] and
//! [`SpectatorMovement`] in `PrePhysics`, the render extract in `Extract`. The extract turns
//! what changed since the last frame into scene updates in the [`RenderExtract`] resource; the
//! engine then loads the meshes it asked for and sends their geometry once loaded.

use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_asset::{AssetServer, BlobStreamer, Handle, LoadState, Uuid, Vfs};
use marlin_containers::{HashMap, HashMapExt};
use marlin_ecs::{Changed, Entity, IntoSystem, With, Without};
use marlin_input::{InputState, Key, MouseButton};
use marlin_io::IoQueue;
use marlin_jobs::{Config, JobSystem};
use marlin_material::DefaultMaterial;
use marlin_math::{DVec3, Quat, Vec3};
use marlin_render::scene::{
    InstanceDesc, InstanceId, InstanceUpdate, LightDesc, LightId, LightKind, LightUpdate,
    MaterialId, MaterialUpdate, MeshId, MeshUpdate,
};
use marlin_render::{SceneIds, SceneUpdates};
use marlin_world::{
    CookedLevel, GlobalTransform, SublevelId, TickGroup, Time, Transform, World, WorldKind,
};

use crate::components::{
    Camera, LevelSettings, MeshRenderer, PointLight, RotatingBase, RotatingMovement, SpectatorLook,
    SpectatorMovement, SpotLight, engine_components,
};
use crate::delta::{MeshUpload, RenderDelta};
use crate::loaders::{LoadedMesh, engine_loaders};

/// The frame's input, for the systems that read it (a world resource).
#[derive(Clone, Copy, Default)]
pub struct FrameInput(pub Option<InputState>);

/// A point or spot light in the render scene's units, at `transform`: lumens to candela
/// (Φ / 4π, a spot's as a point light's), angles to cosines. A spot light shines down -Z.
fn light_desc(
    kind: LightKind,
    color: [f32; 3],
    lumens: f32,
    radius: f32,
    range: f32,
    angles: [f32; 2],
    transform: &marlin_math::Transform,
) -> LightDesc {
    let candela = lumens.max(0.0) / (4.0 * core::f32::consts::PI);
    let outer = angles[1].clamp(0.0, 90.0);
    let inner = angles[0].clamp(0.0, outer);
    let p = transform.translation;
    let d = transform.rotation.mul_vec3(-Vec3::Z).normalize_or_zero();
    LightDesc {
        kind,
        radiance: color.map(|c| c * candela),
        radius: radius.max(0.0),
        position: [p.x, p.y, p.z],
        direction: [d.x, d.y, d.z],
        range: range.max(0.01),
        cone: [
            marlin_math::scalar::cos(inner.to_radians()),
            marlin_math::scalar::cos(outer.to_radians()),
        ],
    }
}

/// A point light's description at `transform`.
pub fn point_light_desc(light: &PointLight, transform: &marlin_math::Transform) -> LightDesc {
    light_desc(
        LightKind::Point,
        light.color,
        light.intensity,
        light.radius,
        light.range,
        [0.0; 2],
        transform,
    )
}

/// A spot light's description at `transform`.
pub fn spot_light_desc(light: &SpotLight, transform: &marlin_math::Transform) -> LightDesc {
    light_desc(
        LightKind::Spot,
        light.color,
        light.intensity,
        light.radius,
        light.range,
        [light.inner_angle, light.outer_angle],
        transform,
    )
}

/// Positions in the render scene are relative to this (0019 §6: a tiled origin comes later).
const RENDER_ORIGIN: DVec3 = DVec3::ZERO;

/// What the render scene has of the world, and the frame's changes to it (a world resource the
/// extract system fills).
pub struct RenderExtract {
    /// Where the scene's rows come from: the render side's scene shares them.
    ids: SceneIds,
    /// Mesh assets in use, by UUID: their row.
    meshes: HashMap<Uuid, MeshId>,
    /// Mesh assets first used this frame: the engine loads them.
    new_meshes: Vec<(Uuid, MeshId)>,
    instances: HashMap<Entity, InstanceEntry>,
    lights: HashMap<Entity, LightId>,
    /// What meshes draw with until they name materials.
    default_material: MaterialId,
    /// The changes since the last delta.
    updates: SceneUpdates,
    camera: Option<(marlin_math::Transform, marlin_render::Camera)>,
}

/// What the render scene has of a drawn entity.
struct InstanceEntry {
    id: InstanceId,
    mesh: MeshId,
    color: [f32; 4],
}

/// `PrePhysics`: turns rotating entities, from the rotation each had when it started.
fn rotate(world: &mut marlin_ecs::World) {
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
fn spectate(world: &mut marlin_ecs::World) {
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
    let Some(state) = world.resource::<FrameInput>().and_then(|i| i.0) else {
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

/// `Extract`: the scene's changes since the last frame (0009 §1.2), into [`RenderExtract`].
/// Instances are entities with a [`GlobalTransform`] and a [`MeshRenderer`]; only those added,
/// changed or removed this frame produce updates, and a moved entity sends just its transform.
fn extract(world: &mut marlin_ecs::World) {
    let Some(mut x) = world.remove_resource::<RenderExtract>() else {
        return;
    };
    let x_ref = &mut x;
    let render = |g: &GlobalTransform| g.relative_to(RENDER_ORIGIN);

    // Entities that lost their mesh, or are gone.
    let gone: Vec<Entity> = world
        .removed::<MeshRenderer>()
        .filter(|&e| !world.has::<MeshRenderer>(e))
        .collect();
    for e in gone {
        if let Some(entry) = x_ref.instances.remove(&e) {
            x_ref
                .updates
                .instances
                .push(entry.id, InstanceUpdate::Remove);
        }
    }
    // New entities, and changed meshes or colors.
    let mut added: Vec<Entity> = Vec::new();
    for (e, g, m) in world
        .query_ref_filtered::<(Entity, &GlobalTransform, &MeshRenderer), Changed<MeshRenderer>>()
    {
        let mesh = match x_ref.meshes.get(&m.mesh.target) {
            Some(&id) => id,
            None => {
                let id = x_ref.ids.meshes.reserve();
                x_ref.updates.meshes.push(id, MeshUpdate::Add);
                x_ref.meshes.insert(m.mesh.target, id);
                x_ref.new_meshes.push((m.mesh.target, id));
                id
            }
        };
        match x_ref.instances.get_mut(&e) {
            None => {
                let id = x_ref.ids.instances.reserve();
                x_ref.updates.instances.push(
                    id,
                    InstanceUpdate::Add(InstanceDesc {
                        mesh,
                        material: x_ref.default_material,
                        transform: render(g),
                        color: m.color,
                        flags: marlin_render::scene::flags::SHADOW,
                    }),
                );
                x_ref.instances.insert(
                    e,
                    InstanceEntry {
                        id,
                        mesh,
                        color: m.color,
                    },
                );
                added.push(e);
            }
            Some(entry) => {
                if entry.mesh != mesh {
                    entry.mesh = mesh;
                    x_ref
                        .updates
                        .instances
                        .push(entry.id, InstanceUpdate::Mesh(mesh));
                }
                if entry.color != m.color {
                    entry.color = m.color;
                    x_ref
                        .updates
                        .instances
                        .push(entry.id, InstanceUpdate::Color(m.color));
                }
            }
        }
    }
    // Moved entities (new ones were added where they are).
    for (e, g) in world
        .query_ref_filtered::<(Entity, &GlobalTransform), (Changed<GlobalTransform>, With<MeshRenderer>)>()
    {
        if let Some(entry) = x_ref.instances.get(&e)
            && !added.contains(&e)
        {
            x_ref
                .updates
                .instances
                .push(entry.id, InstanceUpdate::Transform(render(g)));
        }
    }

    // Lights: those that lost theirs (or are gone) leave the table; new ones, and those whose
    // light or transform changed, are sent whole.
    let has_light = |e| world.has::<PointLight>(e) || world.has::<SpotLight>(e);
    let gone: Vec<Entity> = world
        .removed::<PointLight>()
        .chain(world.removed::<SpotLight>())
        .filter(|&e| !has_light(e))
        .collect();
    for e in gone {
        if let Some(id) = x_ref.lights.remove(&e) {
            x_ref.updates.lights.push(id, LightUpdate::Remove);
        }
    }
    let mut changed: Vec<Entity> = world
        .query_ref_filtered::<Entity, Changed<PointLight>>()
        .chain(world.query_ref_filtered::<Entity, Changed<SpotLight>>())
        .chain(world.query_ref_filtered::<Entity, (Changed<GlobalTransform>, With<PointLight>)>())
        .chain(world.query_ref_filtered::<Entity, (Changed<GlobalTransform>, With<SpotLight>)>())
        .collect();
    changed.sort_unstable();
    changed.dedup();
    for e in changed {
        let Some(g) = world.get::<GlobalTransform>(e) else {
            continue;
        };
        let t = render(g);
        let desc = match (world.get::<PointLight>(e), world.get::<SpotLight>(e)) {
            (_, Some(spot)) => spot_light_desc(spot, &t),
            (Some(point), None) => point_light_desc(point, &t),
            (None, None) => continue,
        };
        match x_ref.lights.get(&e) {
            Some(&id) => x_ref.updates.lights.push(id, LightUpdate::Set(desc)),
            None => {
                let id = x_ref.ids.lights.reserve();
                x_ref.updates.lights.push(id, LightUpdate::Add(desc));
                x_ref.lights.insert(e, id);
            }
        }
    }

    if let Some(settings) = world
        .query_ref_filtered::<&LevelSettings, Changed<LevelSettings>>()
        .next()
    {
        x_ref.updates.environment = Some(settings.to_render());
    }
    x_ref.camera = world
        .query_ref::<(&GlobalTransform, &Camera)>()
        .next()
        .map(|(g, c)| (render(g), c.to_render()));
    world.insert_resource(x);
}

pub struct Engine<'v> {
    vfs: &'v Vfs,
    io: IoQueue<'v>,
    pub assets: AssetServer,
    /// Reads the streamed blobs (mesh pages) the render side asks for, on its own queue.
    pub streamer: Arc<BlobStreamer>,
    stream_io: IoQueue<'v>,
    pub world: World,
    level: Option<Handle<CookedLevel>>,
    /// The level version spawned (`None` until the level loads, or after another is asked for).
    spawned: Option<u32>,
    /// The sublevel the level spawned as.
    sublevel: Option<SublevelId>,
    /// Mesh assets the scene uses: their row and the version last sent to the render side.
    meshes: HashMap<Uuid, MeshEntry>,
    ids: SceneIds,
    /// For callers that pass no job system.
    own_jobs: Option<JobSystem>,
}

/// A mesh asset the scene uses.
struct MeshEntry {
    handle: Handle<LoadedMesh>,
    id: MeshId,
    /// The version last sent to the render side.
    sent: Option<u32>,
}

#[derive(Debug, marlin_error::Error)]
#[error("asset IO")]
pub struct EngineError(#[from] pub marlin_io::Error);

impl<'v> Engine<'v> {
    /// An engine loading from `vfs`, with the engine's loaders and components.
    pub fn new(vfs: &'v Vfs) -> Result<Self, EngineError> {
        let ids = SceneIds::new();
        let default_material = ids.materials.reserve();
        let mut updates = SceneUpdates::default();
        updates.materials.push(
            default_material,
            MaterialUpdate::Add(DefaultMaterial::default().desc()),
        );
        let mut world = World::new(WorldKind::Game, engine_components());
        world.ecs_mut().insert_resource(FrameInput(None));
        world.ecs_mut().insert_resource(RenderExtract {
            ids: ids.clone(),
            meshes: HashMap::new(),
            new_meshes: Vec::new(),
            instances: HashMap::new(),
            lights: HashMap::new(),
            default_material,
            updates,
            camera: None,
        });
        world
            .add_system(
                TickGroup::PrePhysics,
                (rotate as fn(&mut marlin_ecs::World)).named("rotate"),
            )
            .add_system(
                TickGroup::PrePhysics,
                (spectate as fn(&mut marlin_ecs::World)).named("spectate"),
            )
            .add_system(
                TickGroup::Extract,
                (extract as fn(&mut marlin_ecs::World)).named("render_extract"),
            );
        let streamer = Arc::new(BlobStreamer::new());
        Ok(Self {
            vfs,
            io: IoQueue::new(64)?,
            assets: AssetServer::new(engine_loaders(&streamer)),
            streamer,
            stream_io: IoQueue::new(64)?,
            world,
            level: None,
            spawned: None,
            sublevel: None,
            meshes: HashMap::new(),
            ids,
            own_jobs: None,
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

    /// Whether the level and every mesh in use have loaded and been handed to the renderer.
    pub fn is_ready(&self) -> bool {
        self.spawned.is_some()
            && self.meshes.values().all(|m| {
                m.sent.is_some() || !matches!(self.assets.state(&m.handle), LoadState::Loading)
            })
    }

    pub fn frame_count(&self) -> u64 {
        self.world.time().frame()
    }

    /// One frame: loads, (re)spawns the level, runs the world and takes the render delta.
    /// `input` drives spectators; `jobs` runs the world's systems and decompresses loads.
    pub fn frame(
        &mut self,
        dt: f32,
        input: Option<&InputState>,
        jobs: Option<&JobSystem>,
    ) -> RenderDelta {
        self.assets.update(self.vfs, &mut self.io, jobs);
        self.streamer.update(self.vfs, &mut self.stream_io);

        // The level: spawn once loaded, again when it reloads.
        if let Some(level) = &self.level {
            let version = self.assets.version(level);
            if self.spawned != Some(version)
                && let Some(l) = self.assets.get(level)
            {
                let l = Arc::new(l.clone());
                if let Some(old) = self.sublevel.take() {
                    self.world.unload_sublevel(old);
                }
                self.sublevel = Some(self.world.spawn_level(l, Transform::IDENTITY));
                self.spawned = Some(version);
            }
        }

        self.world
            .ecs_mut()
            .insert_resource(FrameInput(input.copied()));
        let jobs = match jobs {
            Some(jobs) => jobs,
            None => self.own_jobs.get_or_insert_with(|| {
                JobSystem::new(Config {
                    workers: 1,
                    ..Config::default()
                })
                .expect("a job system")
            }),
        };
        self.world.frame(dt, jobs);

        let x = self
            .world
            .ecs_mut()
            .resource_mut::<RenderExtract>()
            .expect("the engine's world has a RenderExtract");
        let updates = core::mem::take(&mut x.updates);
        let camera = x.camera;
        let new_meshes = core::mem::take(&mut x.new_meshes);
        for (uuid, id) in new_meshes {
            let handle = self.assets.load(uuid);
            self.meshes.insert(
                uuid,
                MeshEntry {
                    handle,
                    id,
                    sent: None,
                },
            );
        }
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
        RenderDelta {
            frame: self.world.time().frame(),
            dt: self.world.time().delta(),
            updates,
            uploads,
            camera,
        }
    }

    /// The allocators the render scene must take its rows from: give them to
    /// [`RenderSide::new`](crate::RenderSide::new).
    pub fn scene_ids(&self) -> SceneIds {
        self.ids.clone()
    }

    /// Instances the render scene has, as of the last delta.
    pub fn instance_count(&self) -> usize {
        self.world
            .ecs()
            .resource::<RenderExtract>()
            .map_or(0, |x| x.instances.len())
    }
}
