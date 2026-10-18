//! The update streams extract writes and the scene applies (0009 §1.2): per table, a list of
//! (row, change). Jobs each write their own streams; merging appends them, so no lock is needed.

use alloc::vec::Vec;

use marlin_math::Transform;

use super::id::{InstanceId, LightId, MaterialId, MeshId};
use crate::meshes::{Environment, MeshHandle};

/// Changes to one table, in order.
#[derive(Clone, PartialEq, Debug)]
pub struct UpdateStream<I, U> {
    items: Vec<(I, U)>,
}

impl<I, U> UpdateStream<I, U> {
    pub const fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn push(&mut self, id: I, update: U) {
        self.items.push((id, update));
    }

    /// Appends `other`'s changes after this one's.
    pub fn append(&mut self, other: &mut Self) {
        self.items.append(&mut other.items);
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &(I, U)> + '_ {
        self.items.iter()
    }
}

impl<I, U> Default for UpdateStream<I, U> {
    fn default() -> Self {
        Self::new()
    }
}

/// Instance flags (0009 §1.1).
pub mod flags {
    /// Hidden: in the scene, not drawn.
    pub const HIDDEN: u32 = 1;
    /// Casts shadows.
    pub const SHADOW: u32 = 1 << 1;
}

/// A new instance.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct InstanceDesc {
    pub mesh: MeshId,
    /// The material its sections draw with (0011 §7; one for every section until meshes carry
    /// a material per section).
    pub material: MaterialId,
    pub transform: Transform,
    /// Linear RGBA: the material's per-instance data (`SurfaceContext.CustomData`, 0011 §7.3).
    pub color: [f32; 4],
    pub flags: u32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum InstanceUpdate {
    Add(InstanceDesc),
    Remove,
    /// The fast path: only the transform moved.
    Transform(Transform),
    Mesh(MeshId),
    Material(MaterialId),
    Color([f32; 4]),
    Flags(u32),
}

/// A material instance as the GPU Scene holds it (0011 §7.5): which material type's resolve
/// permutation draws it, and its parameter block, the material struct's fields in the shader's
/// layout. Instances of one permutation share a parameter table, so their blocks have one size.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct MaterialDesc {
    pub permutation: u32,
    pub params: Vec<u32>,
}

#[derive(Clone, PartialEq, Debug)]
pub enum MaterialUpdate {
    Add(MaterialDesc),
    /// New parameters, or another permutation.
    Set(MaterialDesc),
    Remove,
}

/// Local bounds of a mesh: a sphere and a box.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Bounds {
    pub center: [f32; 3],
    pub radius: f32,
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Bounds {
    /// The bounds of `positions`; empty input gives a point at the origin.
    pub fn of(positions: impl Iterator<Item = [f32; 3]>) -> Self {
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        let mut any = false;
        let points: Vec<[f32; 3]> = positions.collect();
        for p in &points {
            any = true;
            for i in 0..3 {
                min[i] = min[i].min(p[i]);
                max[i] = max[i].max(p[i]);
            }
        }
        if !any {
            return Self {
                center: [0.0; 3],
                radius: 0.0,
                min: [0.0; 3],
                max: [0.0; 3],
            };
        }
        let center = [0, 1, 2].map(|i| (min[i] + max[i]) * 0.5);
        let radius = points
            .iter()
            .map(|p| {
                let d = [0, 1, 2].map(|i| p[i] - center[i]);
                d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
            })
            .fold(0.0f32, f32::max);
        Self {
            center,
            radius: marlin_math::scalar::sqrt(radius),
            min,
            max,
        }
    }
}

/// A mesh on the GPU, as the GPU Scene's mesh table describes it ([`Meshes::resident_mesh`]
/// gives it).
///
/// [`Meshes::resident_mesh`]: crate::Meshes::resident_mesh
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ResidentMesh {
    /// The resident mesh.
    pub handle: MeshHandle,
    /// The view of the page pool its pages are in (`crate::geometry`).
    pub pages: u32,
    /// The view of its DAG buffer (`crate::geometry`).
    pub dag: u32,
    /// `MeshFlags`.
    pub flags: u32,
    /// Bytes per vertex: positions | attributes << 8 | skin << 16.
    pub strides: u32,
    pub uv_sets: u32,
    /// Quantized positions decode as `offset + q * scale`.
    pub position_offset: [f32; 3],
    pub position_scale: [f32; 3],
    /// Local bounds.
    pub bounds: Bounds,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MeshUpdate {
    /// The mesh is referenced; not on the GPU yet.
    Add,
    /// Its GPU copy, new or replacing the last one (a reload).
    Resident(ResidentMesh),
    Remove,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LightKind {
    Directional,
    Point,
    Spot,
}

/// A light, in physical units (0010 §5.1).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LightDesc {
    pub kind: LightKind,
    /// Linear color times intensity.
    pub radiance: [f32; 3],
    pub position: [f32; 3],
    /// Where it shines, normalized.
    pub direction: [f32; 3],
    pub range: f32,
    /// Spot cone, cosines of the inner and outer half angles.
    pub cone: [f32; 2],
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LightUpdate {
    Add(LightDesc),
    Set(LightDesc),
    Remove,
}

/// One frame's changes to a scene, table by table.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SceneUpdates {
    pub meshes: UpdateStream<MeshId, MeshUpdate>,
    pub materials: UpdateStream<MaterialId, MaterialUpdate>,
    pub instances: UpdateStream<InstanceId, InstanceUpdate>,
    pub lights: UpdateStream<LightId, LightUpdate>,
    pub environment: Option<Environment>,
}

impl SceneUpdates {
    /// Appends `other`'s changes after this one's (one extract job's after another's).
    pub fn append(&mut self, other: &mut Self) {
        self.meshes.append(&mut other.meshes);
        self.materials.append(&mut other.materials);
        self.instances.append(&mut other.instances);
        self.lights.append(&mut other.lights);
        if other.environment.is_some() {
            self.environment = other.environment.take();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.meshes.is_empty()
            && self.materials.is_empty()
            && self.instances.is_empty()
            && self.lights.is_empty()
            && self.environment.is_none()
    }
}
