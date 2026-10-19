//! Extract (0008 §3.2, 0019 §7): what changed in a world since its last frame, as scene updates,
//! and the frame's views.
//!
//! The extract system runs in the world's `Extract` group and writes into the world's
//! [`RenderExtract`] resource. After the world's frame the render module loads the meshes the
//! extract met for the first time, sends the geometry of those loaded, and leaves the frame's
//! [`RenderDelta`] in the world for the host to [`take_delta`].

use alloc::vec::Vec;

use marlin_asset::{AssetServer, Handle, LoadState, Uuid};
use marlin_containers::{HashMap, HashMapExt};
use marlin_ecs::{Changed, Entity, With};
use marlin_material::DefaultMaterial;
use marlin_math::{DVec3, Vec3};
use marlin_render::scene::{
    InstanceDesc, InstanceId, InstanceUpdate, LightDesc, LightId, LightKind, LightUpdate,
    MaterialId, MaterialUpdate, MeshId, MeshUpdate,
};
use marlin_render::{SceneIds, SceneUpdates};
use marlin_world::{EntityGuid, GlobalTransform, World};

use crate::components::{Camera, LevelSettings, MeshRenderer, PointLight, SpotLight};
use crate::delta::{MeshUpload, RenderDelta, RenderView, ViewId};
use crate::meshes::LoadedMesh;

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
pub const RENDER_ORIGIN: DVec3 = DVec3::ZERO;

/// What a world's render scene has of it, and the frame's changes (a world resource).
pub struct RenderExtract {
    /// Where the scene's rows come from: the world renderer's scene shares them.
    ids: SceneIds,
    /// Mesh assets in use, by UUID: their row.
    meshes: HashMap<Uuid, MeshId>,
    /// Mesh assets first used this frame: the module loads them.
    new_meshes: Vec<(Uuid, MeshId)>,
    instances: HashMap<Entity, InstanceEntry>,
    lights: HashMap<Entity, LightId>,
    /// What meshes draw with until they name materials.
    default_material: MaterialId,
    /// The changes since the last delta.
    updates: SceneUpdates,
    views: Vec<RenderView>,
}

impl RenderExtract {
    pub(crate) fn new() -> Self {
        let ids = SceneIds::new();
        let default_material = ids.materials.reserve();
        let mut updates = SceneUpdates::default();
        updates.materials.push(
            default_material,
            MaterialUpdate::Add(DefaultMaterial::default().desc()),
        );
        Self {
            ids,
            meshes: HashMap::new(),
            new_meshes: Vec::new(),
            instances: HashMap::new(),
            lights: HashMap::new(),
            default_material,
            updates,
            views: Vec::new(),
        }
    }

    /// Instances the render scene has, as of the last frame.
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }
}

/// What the scene draws of an entity.
struct InstanceEntry {
    id: InstanceId,
    mesh: MeshId,
    color: [f32; 4],
}

/// A world's mesh assets: loading, and the version last sent to its renderer (a world
/// resource), and the frame's delta once made.
#[derive(Default)]
pub(crate) struct RenderFrame {
    meshes: HashMap<Uuid, MeshEntry>,
    delta: Option<RenderDelta>,
    /// Every mesh in use has loaded (or failed) and been sent.
    ready: bool,
}

struct MeshEntry {
    handle: Handle<LoadedMesh>,
    id: MeshId,
    sent: Option<u32>,
}

/// A camera's view id: its entity's guid if it has one (stable across runs and reloads), else
/// the entity.
fn view_id(e: Entity, guid: Option<&EntityGuid>) -> ViewId {
    match guid {
        Some(g) if !g.is_nil() => ViewId::new(g.0.hash64()),
        _ => ViewId::new(e.to_bits()),
    }
}

/// `Extract`: the scene's changes since the last frame (0009 §1.2) into [`RenderExtract`].
/// Instances are entities with a [`GlobalTransform`] and a [`MeshRenderer`]; only those added,
/// changed or removed this frame produce updates, and a moved entity sends just its transform.
pub fn extract(world: &mut marlin_ecs::World) {
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

    // The views: every camera, highest priority first (ties by id, so the order is stable).
    x_ref.views.clear();
    x_ref.views.extend(
        world
            .query_ref::<(Entity, &GlobalTransform, &Camera, Option<&EntityGuid>)>()
            .map(|(e, g, c, guid)| RenderView {
                id: view_id(e, guid),
                target: c.target,
                priority: c.priority,
                transform: render(g),
                camera: c.to_render(),
            }),
    );
    x_ref
        .views
        .sort_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
    world.insert_resource(x);
}

/// After a world's frame: loads the meshes the extract met first, sends the geometry of those
/// loaded (again when one reloads), and leaves the frame's [`RenderDelta`] in the world.
pub(crate) fn finish_frame(world: &mut World, assets: &mut AssetServer) {
    let frame = world.time().frame();
    let dt = world.time().delta();
    let ecs = world.ecs_mut();
    let Some(x) = ecs.resource_mut::<RenderExtract>() else {
        return;
    };
    let updates = core::mem::take(&mut x.updates);
    let views = x.views.clone();
    let new_meshes = core::mem::take(&mut x.new_meshes);
    let Some(state) = ecs.resource_mut::<RenderFrame>() else {
        return;
    };
    for (uuid, id) in new_meshes {
        let handle = assets.load(uuid);
        state.meshes.insert(
            uuid,
            MeshEntry {
                handle,
                id,
                sent: None,
            },
        );
    }
    let mut uploads = Vec::new();
    for entry in state.meshes.values_mut() {
        if let Some(loaded) = assets.get(&entry.handle) {
            let version = assets.version(&entry.handle);
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
    state.ready = state
        .meshes
        .values()
        .all(|m| m.sent.is_some() || !matches!(assets.state(&m.handle), LoadState::Loading));
    state.delta = Some(RenderDelta {
        frame,
        dt,
        updates,
        uploads,
        views,
    });
}

pub(crate) fn frame_ready(world: &World) -> bool {
    world
        .ecs()
        .resource::<RenderFrame>()
        .is_none_or(|s| s.ready)
}

/// The world's last frame for its renderer; `None` until the world has run a frame since the
/// last take.
pub fn take_delta(world: &mut World) -> Option<RenderDelta> {
    world.ecs_mut().resource_mut::<RenderFrame>()?.delta.take()
}

/// The allocators a world's render scene must take its rows from: give them to
/// [`WorldRenderer::new`](crate::WorldRenderer::new).
pub fn scene_ids(world: &World) -> Option<SceneIds> {
    Some(world.ecs().resource::<RenderExtract>()?.ids.clone())
}

/// Instances a world's render scene has.
pub fn instance_count(world: &World) -> usize {
    world
        .ecs()
        .resource::<RenderExtract>()
        .map_or(0, RenderExtract::instance_count)
}
