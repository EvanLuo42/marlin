//! The render scene (0009 §1–2): what the renderer draws, kept apart from the ECS. Tables of
//! meshes, instances and lights as struct-of-arrays rows with stable handles, changed only by
//! applying [`SceneUpdates`]; and the GPU Scene, their persistent copy on the GPU, brought up to
//! date by one scatter pass a frame that writes only the rows that changed.

mod gpu;
mod id;
mod update;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use marlin_math::{Transform, Vec3};

pub use gpu::{GpuScene, ScatterShader};
pub use id::{
    Id, IdAllocator, InstanceId, InstanceRow, LightId, LightRow, MaterialId, MaterialRow, MeshId,
    MeshRow,
};
pub use update::{
    Bounds, InstanceDesc, InstanceUpdate, LightDesc, LightKind, LightUpdate, MaterialDesc,
    MaterialUpdate, MeshUpdate, ResidentMesh, SceneUpdates, UpdateStream, flags,
};

use crate::meshes::{Environment, MeshHandle};

/// Rows of one table that changed since the GPU Scene was last written, with what changed.
#[derive(Default)]
struct Dirty {
    rows: Vec<u32>,
    bits: Vec<u8>,
}

impl Dirty {
    fn mark(&mut self, row: u32, bits: u8) {
        let i = row as usize;
        if self.bits.len() <= i {
            self.bits.resize(i + 1, 0);
        }
        if self.bits[i] == 0 {
            self.rows.push(row);
        }
        self.bits[i] |= bits;
    }

    fn take(&mut self) -> Vec<(u32, u8)> {
        let out = self
            .rows
            .iter()
            .map(|&r| (r, self.bits[r as usize]))
            .collect();
        for &r in &self.rows {
            self.bits[r as usize] = 0;
        }
        self.rows.clear();
        out
    }
}

/// What changed about an instance row.
pub(crate) mod dirty {
    /// Mesh, flags, color.
    pub const DATA: u8 = 1;
    /// The current transform moved: last frame's becomes the previous one.
    pub const MOVED: u8 = 1 << 1;
    /// The transform stopped: the previous one catches up with the current one.
    pub const STOPPED: u8 = 1 << 2;
    /// Both transforms, written as they are (a new row).
    pub const BOTH: u8 = 1 << 3;
    pub const BOUNDS: u8 = 1 << 4;
}

/// One table's rows: alive or not, and at which generation.
#[derive(Default)]
struct Rows {
    generation: Vec<u32>,
    alive: Vec<bool>,
}

impl Rows {
    /// Makes `id`'s row alive, growing the table; false if the row is taken.
    fn add<T>(&mut self, id: Id<T>) -> bool {
        let i = id.index as usize;
        if self.alive.len() <= i {
            self.alive.resize(i + 1, false);
            self.generation.resize(i + 1, 0);
        }
        if self.alive[i] {
            return false;
        }
        self.alive[i] = true;
        self.generation[i] = id.generation;
        true
    }

    /// Whether `id` names a live row (not a removed one or its successor).
    fn live<T>(&self, id: Id<T>) -> bool {
        let i = id.index as usize;
        i < self.alive.len() && self.alive[i] && self.generation[i] == id.generation
    }

    fn len(&self) -> usize {
        self.alive.len()
    }
}

/// The allocators of a scene's three tables, shared by the scene and whoever produces its
/// updates (extract, on any thread).
#[derive(Clone, Default)]
pub struct SceneIds {
    pub instances: Arc<IdAllocator<InstanceRow>>,
    pub meshes: Arc<IdAllocator<MeshRow>>,
    pub lights: Arc<IdAllocator<LightRow>>,
    pub materials: Arc<IdAllocator<MaterialRow>>,
}

impl SceneIds {
    pub fn new() -> Self {
        Self::default()
    }
}

/// See the module docs.
pub struct RenderScene {
    /// Unique among scenes: keys the GPU Scene's persistent resources.
    id: u64,
    ids: SceneIds,

    instance_rows: Rows,
    instance_mesh: Vec<MeshId>,
    instance_material: Vec<MaterialId>,
    instance_transform: Vec<Transform>,
    /// The transform the last frame drew with (for motion vectors).
    instance_previous: Vec<Transform>,
    instance_color: Vec<[f32; 4]>,
    instance_flags: Vec<u32>,

    material_rows: Rows,
    material_desc: Vec<MaterialDesc>,
    /// Each material's row in its permutation's parameter table.
    material_slot: Vec<u32>,
    /// Per permutation: its parameter table's rows.
    permutation_slots: Vec<Slots>,
    materials_dirty: Dirty,

    mesh_rows: Rows,
    mesh_resident: Vec<Option<ResidentMesh>>,

    light_rows: Rows,
    light_desc: Vec<LightDesc>,

    environment: Environment,

    instances_dirty: Dirty,
    meshes_dirty: Dirty,
    lights_dirty: Dirty,
    /// Frames applied.
    frame: u64,
    /// Per instance row, the last frame its transform changed.
    moved_at: Vec<u64>,
    /// Instances that moved this frame and last frame.
    moved: Vec<u32>,
    moved_last: Vec<u32>,
    /// World spheres whose shadow changed this frame (0010 §5.3): instances that moved,
    /// appeared, vanished, were hidden or shown, or changed mesh (or their mesh its GPU copy),
    /// at their old places and their new ones.
    shadow_changes: Vec<[f32; 4]>,
    /// Rows changed this frame, whose new spheres join `shadow_changes` once the frame's
    /// updates are in; per row, the frame it last changed in.
    shadow_rows: Vec<u32>,
    shadow_at: Vec<u64>,
}

static NEXT_SCENE: AtomicU64 = AtomicU64::new(1);

impl RenderScene {
    pub fn new() -> Self {
        Self::with_ids(SceneIds::new())
    }

    /// A scene whose rows come from `ids`, which the update producers already hold.
    pub fn with_ids(ids: SceneIds) -> Self {
        Self {
            id: NEXT_SCENE.fetch_add(1, Ordering::Relaxed),
            ids,
            instance_rows: Rows::default(),
            instance_mesh: Vec::new(),
            instance_material: Vec::new(),
            instance_transform: Vec::new(),
            instance_previous: Vec::new(),
            instance_color: Vec::new(),
            instance_flags: Vec::new(),
            material_rows: Rows::default(),
            material_desc: Vec::new(),
            material_slot: Vec::new(),
            permutation_slots: Vec::new(),
            materials_dirty: Dirty::default(),
            mesh_rows: Rows::default(),
            mesh_resident: Vec::new(),
            light_rows: Rows::default(),
            light_desc: Vec::new(),
            environment: Environment::default(),
            instances_dirty: Dirty::default(),
            meshes_dirty: Dirty::default(),
            lights_dirty: Dirty::default(),
            frame: 0,
            moved_at: Vec::new(),
            moved: Vec::new(),
            moved_last: Vec::new(),
            shadow_changes: Vec::new(),
            shadow_rows: Vec::new(),
            shadow_at: Vec::new(),
        }
    }

    /// The scene's unique id.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Where update producers reserve rows (from any thread).
    pub fn ids(&self) -> SceneIds {
        self.ids.clone()
    }

    /// Applies one frame's changes: [`apply_all`](Self::apply_all) of just `updates`.
    pub fn apply(&mut self, updates: &SceneUpdates) {
        self.apply_all([updates]);
    }

    /// Applies one frame's changes, set after set; within a set meshes, materials, lights, then
    /// instances, each in stream order. Changes to rows that are not alive (removed, or never
    /// added) are ignored. Rows removed here go back to their allocators. Call it once a frame:
    /// an instance whose transform it does not change comes to rest.
    pub fn apply_all<'a>(&mut self, sets: impl IntoIterator<Item = &'a SceneUpdates>) {
        // A new frame: instances that moved last frame and not again come to rest.
        self.frame += 1;
        self.moved_last = core::mem::take(&mut self.moved);
        self.shadow_changes.clear();
        for updates in sets {
            for (id, update) in updates.meshes.iter() {
                self.apply_mesh(*id, update);
            }
            for (id, update) in updates.materials.iter() {
                self.apply_material(*id, update);
            }
            for (id, update) in updates.lights.iter() {
                self.apply_light(*id, update);
            }
            for (id, update) in updates.instances.iter() {
                self.apply_instance(*id, update);
            }
            if let Some(env) = updates.environment {
                self.environment = env;
            }
        }
        for &row in &self.moved_last {
            if self.instance_rows.alive[row as usize] && self.moved_at[row as usize] != self.frame {
                self.instance_previous[row as usize] = self.instance_transform[row as usize];
                self.instances_dirty.mark(row, dirty::STOPPED);
            }
        }
        for row in core::mem::take(&mut self.shadow_rows) {
            if let Some(sphere) = self.casting_sphere(row as usize) {
                self.shadow_changes.push(sphere);
            }
        }
    }

    fn apply_mesh(&mut self, id: MeshId, update: &MeshUpdate) {
        let i = id.index as usize;
        match *update {
            MeshUpdate::Add => {
                if self.mesh_rows.add(id) {
                    if self.mesh_resident.len() <= i {
                        self.mesh_resident.resize(i + 1, None);
                    }
                    self.mesh_resident[i] = None;
                    self.meshes_dirty.mark(id.index, dirty::DATA);
                }
            }
            MeshUpdate::Resident(resident) if self.mesh_rows.live(id) => {
                self.mesh_changes_shadows(id);
                self.mesh_resident[i] = Some(resident);
                self.meshes_dirty.mark(id.index, dirty::DATA);
                // Instances of the mesh get its bounds.
                for row in 0..self.instance_rows.len() {
                    if self.instance_rows.alive[row] && self.instance_mesh[row] == id {
                        self.instances_dirty.mark(row as u32, dirty::BOUNDS);
                    }
                }
            }
            MeshUpdate::Remove if self.mesh_rows.live(id) => {
                self.mesh_changes_shadows(id);
                self.mesh_rows.alive[i] = false;
                self.mesh_resident[i] = None;
                self.ids.meshes.release(id);
            }
            _ => {}
        }
    }

    fn apply_material(&mut self, id: MaterialId, update: &MaterialUpdate) {
        let i = id.index as usize;
        match update {
            MaterialUpdate::Add(desc) => {
                if !self.material_rows.add(id) {
                    return;
                }
                if self.material_desc.len() <= i {
                    self.material_desc.resize(i + 1, MaterialDesc::default());
                    self.material_slot.resize(i + 1, 0);
                }
                self.material_slot[i] = self.slots(desc.permutation).take();
                self.material_desc[i] = desc.clone();
                self.materials_dirty.mark(id.index, dirty::DATA);
                self.instances_using(id);
            }
            MaterialUpdate::Set(desc) if self.material_rows.live(id) => {
                let old = self.material_desc[i].permutation;
                if old != desc.permutation {
                    let slot = self.material_slot[i];
                    self.slots(old).free.push(slot);
                    self.material_slot[i] = self.slots(desc.permutation).take();
                }
                self.material_desc[i] = desc.clone();
                self.materials_dirty.mark(id.index, dirty::DATA);
            }
            MaterialUpdate::Remove if self.material_rows.live(id) => {
                self.material_rows.alive[i] = false;
                let (permutation, slot) =
                    (self.material_desc[i].permutation, self.material_slot[i]);
                self.slots(permutation).free.push(slot);
                self.ids.materials.release(id);
                self.materials_dirty.mark(id.index, dirty::DATA);
                self.instances_using(id);
            }
            _ => {}
        }
    }

    /// The parameter table rows of `permutation`.
    fn slots(&mut self, permutation: u32) -> &mut Slots {
        let p = permutation as usize;
        if self.permutation_slots.len() <= p {
            self.permutation_slots.resize_with(p + 1, Slots::default);
        }
        &mut self.permutation_slots[p]
    }

    /// Marks the instances drawing with `material` for a rewrite of their GPU row (its material
    /// row there reads as none while the material is not live).
    fn instances_using(&mut self, material: MaterialId) {
        for row in 0..self.instance_rows.len() {
            if self.instance_rows.alive[row] && self.instance_material[row] == material {
                self.instances_dirty.mark(row as u32, dirty::DATA);
            }
        }
    }

    fn apply_light(&mut self, id: LightId, update: &LightUpdate) {
        let i = id.index as usize;
        match *update {
            LightUpdate::Add(desc) => {
                if self.light_rows.add(id) {
                    if self.light_desc.len() <= i {
                        self.light_desc.resize(i + 1, desc);
                    }
                    self.light_desc[i] = desc;
                    self.lights_dirty.mark(id.index, dirty::DATA);
                }
            }
            LightUpdate::Set(desc) if self.light_rows.live(id) => {
                self.light_desc[i] = desc;
                self.lights_dirty.mark(id.index, dirty::DATA);
            }
            LightUpdate::Remove if self.light_rows.live(id) => {
                self.light_rows.alive[i] = false;
                self.ids.lights.release(id);
            }
            _ => {}
        }
    }

    fn apply_instance(&mut self, id: InstanceId, update: &InstanceUpdate) {
        let i = id.index as usize;
        match *update {
            InstanceUpdate::Add(desc) => {
                if !self.instance_rows.add(id) {
                    return;
                }
                if self.instance_mesh.len() <= i {
                    self.instance_mesh.resize(i + 1, desc.mesh);
                    self.instance_material.resize(i + 1, desc.material);
                    self.instance_transform.resize(i + 1, desc.transform);
                    self.instance_previous.resize(i + 1, desc.transform);
                    self.instance_color.resize(i + 1, desc.color);
                    self.instance_flags.resize(i + 1, desc.flags);
                    self.moved_at.resize(i + 1, 0);
                }
                self.instance_mesh[i] = desc.mesh;
                self.instance_material[i] = desc.material;
                self.instance_transform[i] = desc.transform;
                self.instance_previous[i] = desc.transform;
                self.instance_color[i] = desc.color;
                self.instance_flags[i] = desc.flags;
                self.shadow_change(i);
                self.instances_dirty
                    .mark(id.index, dirty::DATA | dirty::BOTH | dirty::BOUNDS);
            }
            _ if !self.instance_rows.live(id) => {}
            InstanceUpdate::Remove => {
                self.shadow_change(i);
                self.instance_rows.alive[i] = false;
                self.ids.instances.release(id);
                // The GPU row reads as hidden from now on.
                self.instances_dirty.mark(id.index, dirty::DATA);
            }
            InstanceUpdate::Transform(t) => {
                self.shadow_change(i);
                if self.moved_at[i] != self.frame {
                    self.moved_at[i] = self.frame;
                    // The transform the last frame drew with.
                    self.instance_previous[i] = self.instance_transform[i];
                    self.moved.push(id.index);
                }
                self.instance_transform[i] = t;
                self.instances_dirty
                    .mark(id.index, dirty::MOVED | dirty::BOUNDS);
            }
            InstanceUpdate::Material(material) => {
                self.instance_material[i] = material;
                self.instances_dirty.mark(id.index, dirty::DATA);
            }
            InstanceUpdate::Mesh(mesh) => {
                self.shadow_change(i);
                self.instance_mesh[i] = mesh;
                self.instances_dirty
                    .mark(id.index, dirty::DATA | dirty::BOUNDS);
            }
            InstanceUpdate::Color(c) => {
                self.instance_color[i] = c;
                self.instances_dirty.mark(id.index, dirty::DATA);
            }
            InstanceUpdate::Flags(f) => {
                self.shadow_change(i);
                self.instance_flags[i] = f;
                self.instances_dirty.mark(id.index, dirty::DATA);
            }
        }
    }

    /// Instance rows, live or not: the GPU tables' rows in use.
    pub(crate) fn instance_rows(&self) -> u32 {
        self.instance_rows.len() as u32
    }

    pub fn environment(&self) -> &Environment {
        &self.environment
    }

    /// Live instances.
    pub fn instance_count(&self) -> usize {
        self.instance_rows.alive.iter().filter(|&&a| a).count()
    }

    pub fn instance(&self, id: InstanceId) -> Option<InstanceDesc> {
        self.instance_rows.live(id).then(|| {
            let i = id.index as usize;
            InstanceDesc {
                mesh: self.instance_mesh[i],
                material: self.instance_material[i],
                transform: self.instance_transform[i],
                color: self.instance_color[i],
                flags: self.instance_flags[i],
            }
        })
    }

    /// The transform `id` was drawn with last frame.
    pub fn previous_transform(&self, id: InstanceId) -> Option<Transform> {
        self.instance_rows
            .live(id)
            .then(|| self.instance_previous[id.index as usize])
    }

    pub fn mesh(&self, id: MeshId) -> Option<Option<ResidentMesh>> {
        self.mesh_rows
            .live(id)
            .then(|| self.mesh_resident[id.index as usize])
    }

    pub fn material(&self, id: MaterialId) -> Option<&MaterialDesc> {
        self.material_rows
            .live(id)
            .then(|| &self.material_desc[id.index as usize])
    }

    pub fn light(&self, id: LightId) -> Option<LightDesc> {
        self.light_rows
            .live(id)
            .then(|| self.light_desc[id.index as usize])
    }

    /// Live instances whose mesh is resident and that are not hidden, as (row, renderer mesh,
    /// transform).
    pub fn drawable(&self) -> impl Iterator<Item = (u32, MeshHandle, Transform)> + '_ {
        (0..self.instance_rows.len()).filter_map(move |row| {
            if !self.instance_rows.alive[row] || self.instance_flags[row] & flags::HIDDEN != 0 {
                return None;
            }
            let mesh = self.instance_mesh[row];
            if !self.mesh_rows.live(mesh) {
                return None;
            }
            self.mesh_resident[mesh.index as usize]
                .map(|m| (row as u32, m.handle, self.instance_transform[row]))
        })
    }

    /// The world spheres whose shadow changed in the last applied frame (see the field).
    pub fn shadow_changes(&self) -> &[[f32; 4]] {
        &self.shadow_changes
    }

    /// The world spheres of the drawable instances of the renderer meshes `meshes`.
    pub fn spheres_of(&self, meshes: &[MeshHandle]) -> impl Iterator<Item = [f32; 4]> + '_ {
        let meshes = meshes.to_vec();
        self.drawable()
            .filter(move |(_, mesh, _)| meshes.contains(mesh))
            .map(|(row, _, _)| self.world_sphere(row as usize))
    }

    /// Instance row `row` changes its shadow this frame: its sphere as it was (the first time
    /// this frame), and its new one once the frame's updates are in.
    fn shadow_change(&mut self, row: usize) {
        if self.shadow_at.len() <= row {
            self.shadow_at.resize(row + 1, 0);
        }
        if self.shadow_at[row] == self.frame {
            return;
        }
        self.shadow_at[row] = self.frame;
        if let Some(sphere) = self.casting_sphere(row) {
            self.shadow_changes.push(sphere);
        }
        self.shadow_rows.push(row as u32);
    }

    /// Every instance of mesh `id` changes its shadow.
    fn mesh_changes_shadows(&mut self, id: MeshId) {
        for row in 0..self.instance_rows.len() {
            if self.instance_rows.alive[row] && self.instance_mesh[row] == id {
                self.shadow_change(row);
            }
        }
    }

    /// The world sphere of instance row `row` if it is drawn: alive, not hidden, its mesh
    /// resident.
    fn casting_sphere(&self, row: usize) -> Option<[f32; 4]> {
        let drawn = self.instance_rows.alive[row]
            && self.instance_flags[row] & flags::HIDDEN == 0
            && self.mesh_rows.live(self.instance_mesh[row])
            && self.mesh_resident[self.instance_mesh[row].index as usize].is_some();
        drawn.then(|| self.world_sphere(row))
    }

    /// The world-space bounding sphere of instance row `row` (zero until its mesh is resident).
    fn world_sphere(&self, row: usize) -> [f32; 4] {
        let mesh = self.instance_mesh[row];
        let Some(Some(resident)) = self
            .mesh_rows
            .live(mesh)
            .then(|| self.mesh_resident[mesh.index as usize])
        else {
            return [0.0; 4];
        };
        let t = self.instance_transform[row];
        let b = resident.bounds;
        let c = t.transform_point(Vec3::new(b.center[0], b.center[1], b.center[2]));
        let s = t.scale.abs();
        let scale = s.x.max(s.y).max(s.z);
        [c.x, c.y, c.z, b.radius * scale]
    }
}

impl Default for RenderScene {
    fn default() -> Self {
        Self::new()
    }
}

/// Rows of one permutation's parameter table.
#[derive(Default)]
struct Slots {
    /// Rows never handed out start here.
    next: u32,
    free: Vec<u32>,
}

impl Slots {
    fn take(&mut self) -> u32 {
        self.free.pop().unwrap_or_else(|| {
            self.next += 1;
            self.next - 1
        })
    }
}
