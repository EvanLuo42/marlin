//! The update streams extract writes and the scene applies (0009 §1.2): per table, a list of
//! (row, change). Jobs each write their own streams; merging appends them, so no lock is needed.

use alloc::vec::Vec;

use marlin_math::Transform;

use super::id::{InstanceId, LightId, MeshId};
use crate::renderer::{Environment, MeshHandle};

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
    pub transform: Transform,
    /// Linear RGBA, until materials (0011) replace it.
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
    Color([f32; 4]),
    Flags(u32),
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

/// What the GPU knows of a resident mesh.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ResidentMesh {
    /// The renderer's mesh: its buffers.
    pub handle: MeshHandle,
    /// Its vertices' view index and index count, for the GPU mesh table.
    pub vertices: u32,
    pub index_count: u32,
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
    pub instances: UpdateStream<InstanceId, InstanceUpdate>,
    pub lights: UpdateStream<LightId, LightUpdate>,
    pub environment: Option<Environment>,
}

impl SceneUpdates {
    /// Appends `other`'s changes after this one's (one extract job's after another's).
    pub fn append(&mut self, other: &mut Self) {
        self.meshes.append(&mut other.meshes);
        self.instances.append(&mut other.instances);
        self.lights.append(&mut other.lights);
        if other.environment.is_some() {
            self.environment = other.environment.take();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.meshes.is_empty()
            && self.instances.is_empty()
            && self.lights.is_empty()
            && self.environment.is_none()
    }
}
