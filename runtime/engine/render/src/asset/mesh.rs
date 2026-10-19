//! Meshes: the source settings authored in `.masset` text and their cooked form.
//!
//! A cooked mesh is a [`CookedMesh`] payload (a marlin-serial cooked layout) holding its
//! [`ClusterDag`]'s resident part, plus bulk blobs: the DAG's pages, the unit of streaming,
//! then one blob for its RT LOD (see [`super::cluster`] for both). Vertex streams share these
//! encodings:
//!
//! - **positions**: 3×u16 quantized over the mesh bounds plus u16 padding
//!   ([`quantize_position`]), or 3×f32 with [`MeshFlags::POSITION_FLOAT`];
//! - **attributes**: normal and tangent in 32 bits ([`encode_tangent_frame`]), then the UV sets
//!   (unorm16 over a cluster's range in pages, 2×f16 in the RT LOD), then RGBA8 color if present;
//! - **skin**: 4×u16 joint indices, 4×unorm8 weights summing to 255.

use alloc::string::String;
use alloc::vec::Vec;

use marlin_anim::Skeleton;
use marlin_asset::{Link, SourceFile};
use marlin_math::scalar::{atan2, cos, sin, sqrt};
pub use marlin_math::scalar::{f16_to_f32, f32_to_f16};
use marlin_reflect::Reflect;
use marlin_serial::cooked::{CookedView, LayoutError, Pod, RelSlice};

use super::cluster::{ClusterDag, ClusterDagView, PageLayout};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Reflect)]
pub enum PositionPrecision {
    /// 16 bits per axis over the bounds: sub-millimeter for objects up to tens of meters.
    #[default]
    Quantized,
    /// For very large objects (terrain pieces, kilometer-scale structures).
    Float,
}

/// A mesh asset: geometry imported from glTF, cooked into a cluster DAG (0009 §3): continuous
/// LOD for the visibility buffer, streamed by page.
///
/// ```text
/// Mesh
/// {
///     !Uuid = "…"
///     !Name = "Rock01"
///     Source = `source:env/rock01.glb`
/// }
/// ```
#[derive(Clone, PartialEq, Debug, Reflect)]
#[reflect(default)]
pub struct Mesh {
    /// A `.gltf` or `.glb`.
    pub source: SourceFile,
    /// The external buffers a `.gltf` refers to.
    pub extra_sources: Vec<SourceFile>,
    /// The glTF mesh to import. Empty: every mesh instanced in the default scene, merged with
    /// its node transforms (skinned meshes are taken as they are bound).
    pub mesh: String,
    pub scale: f32,
    /// The skeleton a skinned mesh's joint indices refer to.
    pub skeleton: Link<Skeleton>,
    /// UV sets kept, from TEXCOORD_0.
    pub max_uv_sets: u32,
    /// Keep COLOR_0 when present.
    pub vertex_colors: bool,
    pub precision: PositionPrecision,
    /// The share of the source triangles the DAG's RT LOD keeps (ray tracing builds its BLAS
    /// from it).
    pub rt_fraction: f32,
}

impl Default for Mesh {
    fn default() -> Self {
        Self {
            source: SourceFile::default(),
            extra_sources: Vec::new(),
            mesh: String::new(),
            scale: 1.0,
            skeleton: Link::NONE,
            max_uv_sets: 2,
            vertex_colors: true,
            precision: PositionPrecision::Quantized,
            rt_fraction: 0.25,
        }
    }
}

/// [`CookedMesh::flags`] bits.
pub struct MeshFlags;

impl MeshFlags {
    /// Positions are 3×f32 instead of quantized.
    pub const POSITION_FLOAT: u32 = 1;
    pub const SKIN: u32 = 2;
    pub const COLOR: u32 = 4;
}

/// The payload root of a cooked mesh.
#[derive(Pod, Clone, Copy, Debug)]
#[repr(C)]
pub struct CookedMesh {
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    /// Bounding sphere center and radius.
    pub sphere: [f32; 4],
    /// Quantized positions decode as `offset + q * scale` per axis.
    pub position_offset: [f32; 3],
    pub position_scale: [f32; 3],
    /// [`MeshFlags`].
    pub flags: u32,
    pub uv_sets: u32,
    pub position_stride: u32,
    pub attribute_stride: u32,
    /// 12 with [`MeshFlags::SKIN`], else 0.
    pub skin_stride: u32,
    /// Material slots, in glTF material order of first use.
    pub sections: RelSlice<MeshSection>,
    /// The skin's joints, which skin indices refer to (empty without [`MeshFlags::SKIN`]).
    pub joints: RelSlice<MeshJoint>,
    pub cluster_dag: ClusterDag,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MeshSection {
    /// The source material's name: what material assignments match on.
    pub name: RelSlice<u8>,
}

// SAFETY: a RelSlice: 8 bytes, no padding.
unsafe impl Pod for MeshSection {}

/// A joint of a skinned mesh's skin, bound to a skeleton joint of the same name at load.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MeshJoint {
    pub name: RelSlice<u8>,
    /// Mesh space to the joint's space at bind time, column-major.
    pub inverse_bind: [f32; 16],
}

// SAFETY: a RelSlice and 16 f32: 72 bytes, align 4, no padding.
unsafe impl Pod for MeshJoint {}

/// A byte range in a bulk blob.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(C)]
pub struct PageRange {
    pub offset: u32,
    pub size: u32,
}

// SAFETY: two u32, no padding.
unsafe impl Pod for PageRange {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MeshError {
    Layout(LayoutError),
    /// Counts, strides or ranges that do not describe a mesh.
    Malformed,
}

impl From<LayoutError> for MeshError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// A checked cooked mesh.
#[derive(Clone, Copy, Debug)]
pub struct MeshView<'a> {
    view: CookedView<'a>,
    pub header: &'a CookedMesh,
    pub sections: &'a [MeshSection],
    pub joints: &'a [MeshJoint],
    /// The cluster DAG, checked but for its pages ([`ClusterDagView::page`]) and its RT LOD's
    /// blob ([`ClusterDagView::check_rt`]).
    pub cluster_dag: ClusterDagView<'a>,
}

impl<'a> MeshView<'a> {
    /// Reads `payload` and checks the DAG's resident part against itself and the bulk blobs'
    /// sizes `bulk_sizes`. Pages are checked by [`ClusterDagView::page`] once loaded.
    pub fn new(payload: &'a [u8], bulk_sizes: &[u64]) -> Result<Self, MeshError> {
        let view = CookedView::new(payload);
        let header: &CookedMesh = view.root()?;
        let sections = view.slice(&header.sections)?;
        let joints = view.slice(&header.joints)?;
        let float = header.flags & MeshFlags::POSITION_FLOAT != 0;
        let skin = header.flags & MeshFlags::SKIN != 0;
        let color = header.flags & MeshFlags::COLOR != 0;
        // The UV set count first: the attribute stride is computed from it.
        let stride_ok = header.uv_sets <= 4
            && header.position_stride == if float { 12 } else { 8 }
            && header.attribute_stride == 4 + 4 * header.uv_sets + if color { 4 } else { 0 }
            && header.skin_stride == if skin { 12 } else { 0 };
        if !stride_ok || sections.is_empty() || skin == joints.is_empty() {
            return Err(MeshError::Malformed);
        }
        for j in joints {
            view.str(&j.name)?;
        }
        for s in sections {
            view.str(&s.name)?;
        }
        let cluster_dag = ClusterDagView::new(
            &view,
            &header.cluster_dag,
            PageLayout {
                uv_sets: header.uv_sets,
                position_stride: header.position_stride,
                attribute_stride: header.attribute_stride,
                skin_stride: header.skin_stride,
                mesh_sections: sections.len() as u32,
                joints: joints.len() as u32,
            },
            bulk_sizes,
        )?;
        Ok(Self {
            view,
            header,
            sections,
            joints,
            cluster_dag,
        })
    }

    pub fn section_name(&self, section: usize) -> &'a str {
        // Checked in `new`.
        self.view
            .str(&self.sections[section].name)
            .unwrap_or_default()
    }
}

/// An orthonormal basis around unit `n` (Duff et al., "Building an Orthonormal Basis,
/// Revisited", 2017): continuous everywhere but at the seam it is built to put out of the way,
/// and branch-free but for the sign. Shaders build the same basis to decode tangents.
pub fn basis(n: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let sign = if n[2] >= 0.0 { 1.0 } else { -1.0 };
    let a = -1.0 / (sign + n[2]);
    let b = n[0] * n[1] * a;
    (
        [1.0 + sign * n[0] * n[0] * a, sign * b, -sign * n[0]],
        [b, sign + n[1] * n[1] * a, -n[1]],
    )
}

fn oct_encode(n: [f32; 3]) -> [f32; 2] {
    let l1 = n[0].abs() + n[1].abs() + n[2].abs();
    let (x, y) = (n[0] / l1, n[1] / l1);
    if n[2] >= 0.0 {
        [x, y]
    } else {
        let sx = if x >= 0.0 { 1.0 } else { -1.0 };
        let sy = if y >= 0.0 { 1.0 } else { -1.0 };
        [(1.0 - y.abs()) * sx, (1.0 - x.abs()) * sy]
    }
}

fn oct_decode(e: [f32; 2]) -> [f32; 3] {
    let z = 1.0 - e[0].abs() - e[1].abs();
    let t = (-z).max(0.0);
    let x = e[0] + if e[0] >= 0.0 { -t } else { t };
    let y = e[1] + if e[1] >= 0.0 { -t } else { t };
    normalize([x, y, z])
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = sqrt(v[0] * v[0] + v[1] * v[1] + v[2] * v[2]);
    if len > 0.0 {
        v.map(|c| c / len)
    } else {
        [0.0, 0.0, 1.0]
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

const TAU: f32 = core::f32::consts::TAU;

/// Packs a unit normal, a unit tangent (orthogonal to it) and the bitangent sign into 32 bits:
///
/// - bits 0–9 and 10–19: the normal, octahedral-encoded, each coordinate unorm10 over [-1, 1];
/// - bits 20–30: the tangent's angle in the plane of the decoded normal, measured from the
///   first vector of [`basis`], unorm11 over [0, 2π);
/// - bit 31: set when the bitangent is `-cross(normal, tangent)`.
///
/// The tangent is encoded against the *decoded* normal, so decoding rebuilds the same basis.
pub fn encode_tangent_frame(normal: [f32; 3], tangent: [f32; 3], bitangent_sign: f32) -> u32 {
    let q = |v: f32| ((v.clamp(-1.0, 1.0) * 0.5 + 0.5) * 1023.0 + 0.5) as u32;
    let oct = oct_encode(normalize(normal));
    let (nx, ny) = (q(oct[0]), q(oct[1]));
    let n = oct_decode([
        nx as f32 / 1023.0 * 2.0 - 1.0,
        ny as f32 / 1023.0 * 2.0 - 1.0,
    ]);
    let (b0, b1) = basis(n);
    // Project the tangent into the decoded normal's plane before measuring its angle.
    let t = tangent;
    let d = dot(t, n);
    let t = [t[0] - n[0] * d, t[1] - n[1] * d, t[2] - n[2] * d];
    let mut angle = atan2(dot(t, b1), dot(t, b0));
    if angle < 0.0 {
        angle += TAU;
    }
    let a = ((angle / TAU * 2048.0 + 0.5) as u32) & 2047;
    let sign = u32::from(bitangent_sign < 0.0);
    nx | ny << 10 | a << 20 | sign << 31
}

/// Unpacks [`encode_tangent_frame`]: normal, tangent and bitangent sign (±1).
pub fn decode_tangent_frame(bits: u32) -> ([f32; 3], [f32; 3], f32) {
    let c = |v: u32| (v & 1023) as f32 / 1023.0 * 2.0 - 1.0;
    let n = oct_decode([c(bits), c(bits >> 10)]);
    let (b0, b1) = basis(n);
    let angle = ((bits >> 20) & 2047) as f32 / 2048.0 * TAU;
    let (s, co) = (sin(angle), cos(angle));
    let t = core::array::from_fn(|i| b0[i] * co + b1[i] * s);
    (n, t, if bits >> 31 != 0 { -1.0 } else { 1.0 })
}

/// A quantized position in `[0, 65535]` per axis over `bounds_min..bounds_max`, and the
/// [`CookedMesh::position_scale`] that decodes it.
pub fn quantize_position(p: [f32; 3], min: [f32; 3], scale: [f32; 3]) -> [u16; 3] {
    core::array::from_fn(|i| {
        if scale[i] > 0.0 {
            ((p[i] - min[i]) / scale[i] + 0.5).clamp(0.0, 65535.0) as u16
        } else {
            0
        }
    })
}

pub fn dequantize_position(q: [u16; 3], offset: [f32; 3], scale: [f32; 3]) -> [f32; 3] {
    core::array::from_fn(|i| offset[i] + q[i] as f32 * scale[i])
}

/// The decode scale for positions over `min..max`.
pub fn position_scale(min: [f32; 3], max: [f32; 3]) -> [f32; 3] {
    core::array::from_fn(|i| (max[i] - min[i]).max(0.0) / 65535.0)
}

/// Reads a vertex's normal, tangent frame and UVs from an attribute stream (for tools and CPU
/// paths; shaders decode the same layout).
pub fn read_attributes(
    attributes: &[u8],
    stride: usize,
    vertex: usize,
    uv_sets: usize,
) -> VertexAttributes {
    let v = &attributes[vertex * stride..][..stride];
    let frame = u32::from_le_bytes([v[0], v[1], v[2], v[3]]);
    let (normal, tangent, sign) = decode_tangent_frame(frame);
    let mut uvs = [[0.0; 2]; 4];
    for (set, uv) in uvs.iter_mut().enumerate().take(uv_sets) {
        let at = 4 + set * 4;
        *uv = [
            f16_to_f32(u16::from_le_bytes([v[at], v[at + 1]])),
            f16_to_f32(u16::from_le_bytes([v[at + 2], v[at + 3]])),
        ];
    }
    let at = 4 + uv_sets * 4;
    let color = (stride >= at + 4).then(|| [v[at], v[at + 1], v[at + 2], v[at + 3]]);
    VertexAttributes {
        normal,
        tangent,
        bitangent_sign: sign,
        uvs,
        color,
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VertexAttributes {
    pub normal: [f32; 3],
    pub tangent: [f32; 3],
    pub bitangent_sign: f32,
    pub uvs: [[f32; 2]; 4],
    pub color: Option<[u8; 4]>,
}
