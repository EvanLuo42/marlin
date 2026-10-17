//! Meshes: the source settings authored in `.masset` text and their cooked form.
//!
//! A cooked mesh is a [`CookedMesh`] payload (a marlin-serial cooked layout) plus one bulk blob
//! per LOD, the LOD's streaming page. A page holds, each 16-byte aligned:
//!
//! | stream              | per element                                                   |
//! |---------------------|---------------------------------------------------------------|
//! | positions           | 3×u16 quantized over the mesh bounds plus u16 padding, or 3×f32 |
//! | attributes          | normal and tangent in 32 bits ([`encode_tangent_frame`]), then 2×f16 per UV set, then RGBA8 color if present |
//! | skin                | 4×u16 joint indices, 4×unorm8 weights summing to 255           |
//! | meshlets            | [`Meshlet`]                                                    |
//! | meshlet vertices    | u32 index into the LOD's vertex streams                        |
//! | meshlet triangles   | 3×u8 indices into the meshlet's vertices                       |
//! | indices             | the LOD's triangle list (u16 or u32), for BLAS builds and paths without mesh shaders |

use alloc::string::String;
use alloc::vec::Vec;

use marlin_anim::Skeleton;
use marlin_asset::{Link, SourceFile};
use marlin_math::scalar::{atan2, cos, sin, sqrt};
use marlin_reflect::Reflect;
use marlin_serial::cooked::{CookedView, LayoutError, Pod, RelSlice};

/// How the LOD chain is generated.
#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(default)]
pub struct LodSettings {
    /// LODs including LOD 0.
    pub count: u32,
    /// Triangle count of each LOD relative to the one before.
    pub reduction: f32,
    /// Stop when a LOD's error would exceed this fraction of the mesh's bounding radius.
    pub max_error: f32,
}

impl Default for LodSettings {
    fn default() -> Self {
        Self {
            count: 4,
            reduction: 0.5,
            max_error: 0.05,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Reflect)]
pub enum PositionPrecision {
    /// 16 bits per axis over the bounds: sub-millimeter for objects up to tens of meters.
    #[default]
    Quantized,
    /// For very large objects (terrain pieces, kilometer-scale structures).
    Float,
}

/// A mesh asset: geometry imported from glTF.
///
/// ```text
/// Mesh
/// {
///     !Uuid = "…"
///     !Name = "Rock01"
///     Source = `source:env/rock01.glb`
///     Lods = { Count = 5 }
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
    pub lods: LodSettings,
    /// The skeleton a skinned mesh's joint indices refer to.
    pub skeleton: Link<Skeleton>,
    /// UV sets kept, from TEXCOORD_0.
    pub max_uv_sets: u32,
    /// Keep COLOR_0 when present.
    pub vertex_colors: bool,
    pub precision: PositionPrecision,
    /// The LOD ray tracing builds its BLAS from. `None`: one coarser than the LOD whose error
    /// reaches a pixel at the typical viewing distance, picked by the cooker.
    pub rt_lod: Option<u32>,
}

impl Default for Mesh {
    fn default() -> Self {
        Self {
            source: SourceFile::default(),
            extra_sources: Vec::new(),
            mesh: String::new(),
            scale: 1.0,
            lods: LodSettings::default(),
            skeleton: Link::NONE,
            max_uv_sets: 2,
            vertex_colors: true,
            precision: PositionPrecision::Quantized,
            rt_lod: None,
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

/// Most vertices a meshlet references.
pub const MESHLET_MAX_VERTICES: u32 = 64;
/// Most triangles in a meshlet (124 keeps the triangle data of a meshlet in 372 bytes, and
/// leaves mesh shader output room on every vendor).
pub const MESHLET_MAX_TRIANGLES: u32 = 124;

/// The payload root of a cooked mesh.
#[derive(Clone, Copy, Debug)]
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
    pub rt_lod: u32,
    /// Material slots, in glTF material order of first use.
    pub sections: RelSlice<MeshSection>,
    /// Finest first.
    pub lods: RelSlice<MeshLod>,
    /// The skin's joints, which skin indices refer to (empty without [`MeshFlags::SKIN`]).
    pub joints: RelSlice<MeshJoint>,
}

// SAFETY: 22 4-byte scalars and three RelSlices: 112 bytes, align 4, no padding.
unsafe impl Pod for CookedMesh {}

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

/// A byte range in a LOD page.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(C)]
pub struct PageRange {
    pub offset: u32,
    pub size: u32,
}

// SAFETY: two u32, no padding.
unsafe impl Pod for PageRange {}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MeshLod {
    /// Index of the page among the object's bulk blobs.
    pub bulk: u32,
    pub vertex_count: u32,
    pub triangle_count: u32,
    pub meshlet_count: u32,
    /// Object-space simplification error (a distance). LOD 0's is 0.
    pub error: f32,
    /// Bytes per index: 2 or 4.
    pub index_size: u32,
    pub positions: PageRange,
    pub attributes: PageRange,
    pub skin: PageRange,
    pub meshlets: PageRange,
    pub meshlet_vertices: PageRange,
    pub meshlet_triangles: PageRange,
    pub indices: PageRange,
    /// One per mesh section: its meshlets and indices in this LOD.
    pub sections: RelSlice<LodSection>,
}

// SAFETY: six 4-byte scalars, seven PageRanges and a RelSlice: 88 bytes, no padding.
unsafe impl Pod for MeshLod {}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(C)]
pub struct LodSection {
    pub first_meshlet: u32,
    pub meshlet_count: u32,
    pub first_index: u32,
    pub index_count: u32,
}

// SAFETY: four u32, no padding.
unsafe impl Pod for LodSection {}

/// A cluster of up to [`MESHLET_MAX_TRIANGLES`] triangles over up to [`MESHLET_MAX_VERTICES`]
/// vertices, as the culling and mesh shaders read it.
///
/// The cone test (back-facing clusters): with `d = center - camera`, the meshlet is culled when
/// `dot(d, axis) >= cutoff * length(d) + radius`, with `axis` and `cutoff` the snorm8 `cone`.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
#[repr(C)]
pub struct Meshlet {
    pub center: [f32; 3],
    pub radius: f32,
    /// Cone axis xyz and cutoff, snorm8. A cutoff of 127 means the cone is degenerate (never
    /// culled).
    pub cone: [i8; 4],
    /// First entry in the page's meshlet vertices.
    pub vertex_offset: u32,
    /// Byte offset of the first triangle in the page's meshlet triangles.
    pub triangle_offset: u32,
    pub vertex_count: u8,
    pub triangle_count: u8,
    pub section: u16,
    /// DAG fields. With discrete LODs: `group` is the meshlet's LOD, `lod_error` the
    /// LOD's error, `parent_error` +∞ and `error_sphere` the mesh's bounding sphere.
    pub group: u32,
    pub lod_error: f32,
    pub parent_error: f32,
    pub error_sphere: [f32; 4],
    pub lod: u32,
}

// SAFETY: 16 + 4 + 4 + 4 + (1 + 1 + 2) + 4 + 4 + 4 + 16 + 4 = 64 bytes, align 4, every field
// at its natural alignment, no padding; any bits are valid.
unsafe impl Pod for Meshlet {}

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
    pub lods: &'a [MeshLod],
    pub joints: &'a [MeshJoint],
}

impl<'a> MeshView<'a> {
    /// Reads `payload` and checks every LOD's streams against its page size in `bulk_sizes`
    /// (indexed by [`MeshLod::bulk`]). Page contents are checked by [`check_page`] once loaded.
    ///
    /// [`check_page`]: MeshView::check_page
    pub fn new(payload: &'a [u8], bulk_sizes: &[u64]) -> Result<Self, MeshError> {
        let view = CookedView::new(payload);
        let header: &CookedMesh = view.root()?;
        let sections = view.slice(&header.sections)?;
        let lods = view.slice(&header.lods)?;
        let joints = view.slice(&header.joints)?;
        let float = header.flags & MeshFlags::POSITION_FLOAT != 0;
        let skin = header.flags & MeshFlags::SKIN != 0;
        let color = header.flags & MeshFlags::COLOR != 0;
        let stride_ok = header.position_stride == if float { 12 } else { 8 }
            && header.attribute_stride == 4 + 4 * header.uv_sets + if color { 4 } else { 0 }
            && header.skin_stride == if skin { 12 } else { 0 };
        if !stride_ok
            || lods.is_empty()
            || sections.is_empty()
            || header.rt_lod as usize >= lods.len()
            || header.uv_sets > 4
            || skin == joints.is_empty()
        {
            return Err(MeshError::Malformed);
        }
        for j in joints {
            view.str(&j.name)?;
        }
        for s in sections {
            view.str(&s.name)?;
        }
        for lod in lods {
            let size = *bulk_sizes
                .get(lod.bulk as usize)
                .ok_or(MeshError::Malformed)?;
            let fits = |r: &PageRange, len: u64| {
                r.size as u64 == len
                    && r.offset as u64 + r.size as u64 <= size
                    && r.offset % 16 == 0
            };
            let v = lod.vertex_count as u64;
            let ok = fits(&lod.positions, v * header.position_stride as u64)
                && fits(&lod.attributes, v * header.attribute_stride as u64)
                && fits(&lod.skin, v * header.skin_stride as u64)
                && fits(&lod.meshlets, lod.meshlet_count as u64 * 64)
                && fits(&lod.meshlet_vertices, lod.meshlet_vertices.size as u64)
                && lod.meshlet_vertices.size % 4 == 0
                && fits(&lod.meshlet_triangles, lod.meshlet_triangles.size as u64)
                && (lod.index_size == 2 || lod.index_size == 4)
                && fits(
                    &lod.indices,
                    lod.triangle_count as u64 * 3 * lod.index_size as u64,
                );
            let lod_sections = view.slice(&lod.sections)?;
            if !ok || lod_sections.len() != sections.len() {
                return Err(MeshError::Malformed);
            }
            let mut meshlets = 0u64;
            let mut indices = 0u64;
            for s in lod_sections {
                if s.first_meshlet as u64 != meshlets || s.first_index as u64 != indices {
                    return Err(MeshError::Malformed);
                }
                meshlets += s.meshlet_count as u64;
                indices += s.index_count as u64;
            }
            if meshlets != lod.meshlet_count as u64 || indices != lod.triangle_count as u64 * 3 {
                return Err(MeshError::Malformed);
            }
        }
        Ok(Self {
            view,
            header,
            sections,
            lods,
            joints,
        })
    }

    pub fn section_name(&self, section: usize) -> &'a str {
        // Checked in `new`.
        self.view
            .str(&self.sections[section].name)
            .unwrap_or_default()
    }

    pub fn lod_sections(&self, lod: usize) -> &'a [LodSection] {
        // Checked in `new`.
        self.view
            .slice(&self.lods[lod].sections)
            .unwrap_or_default()
    }

    /// Checks a loaded page: every meshlet's vertices and triangles in range, every index below
    /// the vertex count. The GPU reads pages unchecked, so a page is checked once on load.
    pub fn check_page(&self, lod: usize, page: &[u8]) -> Result<(), MeshError> {
        let lod = &self.lods[lod];
        let range =
            |r: &PageRange| page.get(r.offset as usize..r.offset as usize + r.size as usize);
        let (Some(meshlets), Some(vertices), Some(triangles), Some(indices), Some(skin)) = (
            range(&lod.meshlets),
            range(&lod.meshlet_vertices),
            range(&lod.meshlet_triangles),
            range(&lod.indices),
            range(&lod.skin),
        ) else {
            return Err(MeshError::Malformed);
        };
        let word = |b: &[u8], i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        for m in meshlets.as_chunks::<64>().0 {
            let vertex_offset = word(m, 20) as usize;
            let triangle_offset = word(m, 24) as usize;
            let (vertex_count, triangle_count) = (m[28] as usize, m[29] as usize);
            if vertex_count as u32 > MESHLET_MAX_VERTICES
                || triangle_count as u32 > MESHLET_MAX_TRIANGLES
                || (vertex_offset + vertex_count) * 4 > vertices.len()
                || triangle_offset + triangle_count * 3 > triangles.len()
                || triangles[triangle_offset..][..triangle_count * 3]
                    .iter()
                    .any(|&t| t as usize >= vertex_count)
                || (0..vertex_count)
                    .any(|i| word(vertices, (vertex_offset + i) * 4) >= lod.vertex_count)
            {
                return Err(MeshError::Malformed);
            }
        }
        let bad_index = if lod.index_size == 2 {
            indices
                .as_chunks::<2>()
                .0
                .iter()
                .any(|i| u16::from_le_bytes([i[0], i[1]]) as u32 >= lod.vertex_count)
        } else {
            indices
                .as_chunks::<4>()
                .0
                .iter()
                .any(|i| word(i, 0) >= lod.vertex_count)
        };
        let joints = self.joints.len() as u16;
        let bad_joint = skin.as_chunks::<12>().0.iter().any(|v| {
            (0..4).any(|i| u16::from_le_bytes([v[i * 2], v[i * 2 + 1]]) >= joints && v[8 + i] != 0)
        });
        if bad_index || bad_joint {
            return Err(MeshError::Malformed);
        }
        Ok(())
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

/// IEEE half to single precision.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = match (exp, man) {
        (0, 0) => sign,
        (0, _) => {
            // Subnormal: normalize.
            let shift = man.leading_zeros() - 21;
            let man = (man << shift) & 0x3ff;
            sign | (113 - shift) << 23 | man << 13
        }
        (31, 0) => sign | 0x7f80_0000,
        (31, _) => sign | 0x7fc0_0000 | man << 13,
        _ => sign | (exp + 112) << 23 | man << 13,
    };
    f32::from_bits(bits)
}

/// Single to half precision, rounding to nearest even.
pub fn f32_to_f16(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let man = bits & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 31 {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let man = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = man >> shift;
        let rest = man & ((1 << shift) - 1);
        let mid = 1 << (shift - 1);
        let round = u32::from(rest > mid || (rest == mid && half & 1 != 0));
        return sign | (half + round) as u16;
    }
    let half = ((e as u32) << 10) | (man >> 13);
    let rest = man & 0x1fff;
    let round = u32::from(rest > 0x1000 || (rest == 0x1000 && half & 1 != 0));
    sign | (half + round) as u16
}

impl MeshView<'_> {
    /// LOD `lod`'s triangles in the forward renderer's form: positions and normals, 32-bit
    /// indices. A CPU decode of its page (checked first), for the forward path until the
    /// visibility buffer consumes pages as they are.
    pub fn decode_lod(
        &self,
        lod: usize,
        page: &[u8],
    ) -> Result<(Vec<crate::mesh::Vertex>, Vec<u32>), MeshError> {
        self.check_page(lod, page)?;
        let h = self.header;
        let l = &self.lods[lod];
        let slice = |r: &PageRange| &page[r.offset as usize..][..r.size as usize];
        let (positions, attributes, indices) =
            (slice(&l.positions), slice(&l.attributes), slice(&l.indices));
        let float = h.flags & MeshFlags::POSITION_FLOAT != 0;
        let f32_at =
            |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        let vertices = (0..l.vertex_count as usize)
            .map(|v| {
                let position = if float {
                    core::array::from_fn(|i| f32_at(positions, v * 12 + i * 4))
                } else {
                    let q = core::array::from_fn(|i| {
                        u16::from_le_bytes([positions[v * 8 + i * 2], positions[v * 8 + i * 2 + 1]])
                    });
                    dequantize_position(q, h.position_offset, h.position_scale)
                };
                let normal = read_attributes(attributes, h.attribute_stride as usize, v, 0).normal;
                crate::mesh::Vertex { position, normal }
            })
            .collect();
        let indices = if l.index_size == 2 {
            indices
                .as_chunks::<2>()
                .0
                .iter()
                .map(|i| u32::from(u16::from_le_bytes([i[0], i[1]])))
                .collect()
        } else {
            indices
                .as_chunks::<4>()
                .0
                .iter()
                .map(|i| u32::from_le_bytes([i[0], i[1], i[2], i[3]]))
                .collect()
        };
        Ok((vertices, indices))
    }
}
