//! The cluster DAG of a cooked mesh (0009 §3): clusters of up to [`CLUSTER_MAX_TRIANGLES`]
//! triangles grouped into a DAG of continuous LOD, packed into streaming pages.
//!
//! # Vertices
//!
//! A cluster's vertices are its own (a vertex shared by two clusters is stored twice), so a
//! cluster decodes without anything outside its page:
//!
//! - **positions**: as the mesh's ([`quantize_position`] over the mesh bounds, or 3×f32 with
//!   `MeshFlags::POSITION_FLOAT`). Quantizing over the whole mesh, not per cluster, keeps a
//!   position shared by two clusters bit-identical in both: LOD cuts stay watertight;
//! - **normal and tangent**: [`encode_tangent_frame`] (octahedral normal, tangent angle, sign);
//! - **UVs**: unorm16 per coordinate over the cluster's range of each set ([`UvRange`]), so
//!   tiling UVs keep their precision;
//! - **color**: RGBA8; **skin**: 4×u16 joints and 4×unorm8 weights, as the mesh's.
//!
//! [`quantize_position`]: super::mesh::quantize_position
//! [`encode_tangent_frame`]: super::mesh::encode_tangent_frame

use marlin_serial::cooked::Pod;

use super::mesh::{VertexAttributes, decode_tangent_frame};

/// Most triangles in a cluster: within mesh shader output limits on every vendor and under the
/// 256 of DXR cluster acceleration structures, so a cluster can become a CLAS later.
pub const CLUSTER_MAX_TRIANGLES: u32 = 128;
/// Most vertices a cluster references.
pub const CLUSTER_MAX_VERTICES: u32 = 128;

/// How one UV set of a cluster decodes: `offset + q * scale` per coordinate.
#[derive(Pod, Clone, Copy, PartialEq, Debug, Default)]
#[repr(C)]
pub struct UvRange {
    pub offset: [f32; 2],
    pub scale: [f32; 2],
}

impl UvRange {
    /// The range covering `uvs`.
    pub fn of(uvs: impl IntoIterator<Item = [f32; 2]>) -> Self {
        let mut lo = [f32::MAX; 2];
        let mut hi = [f32::MIN; 2];
        for uv in uvs {
            for i in 0..2 {
                lo[i] = lo[i].min(uv[i]);
                hi[i] = hi[i].max(uv[i]);
            }
        }
        if lo[0] > hi[0] {
            return Self::default();
        }
        Self {
            offset: lo,
            scale: core::array::from_fn(|i| (hi[i] - lo[i]).max(0.0) / 65535.0),
        }
    }

    pub fn quantize(&self, uv: [f32; 2]) -> [u16; 2] {
        core::array::from_fn(|i| {
            if self.scale[i] > 0.0 {
                ((uv[i] - self.offset[i]) / self.scale[i] + 0.5).clamp(0.0, 65535.0) as u16
            } else {
                0
            }
        })
    }

    pub fn dequantize(&self, q: [u16; 2]) -> [f32; 2] {
        core::array::from_fn(|i| self.offset[i] + q[i] as f32 * self.scale[i])
    }
}

/// Reads a cluster vertex's normal, tangent frame, UVs and color from a page's attribute stream
/// (for tools and CPU paths; shaders decode the same layout). `ranges` are the cluster's, one per
/// UV set.
pub fn read_cluster_attributes(
    attributes: &[u8],
    stride: usize,
    vertex: usize,
    ranges: &[UvRange],
) -> VertexAttributes {
    let v = &attributes[vertex * stride..][..stride];
    let frame = u32::from_le_bytes([v[0], v[1], v[2], v[3]]);
    let (normal, tangent, sign) = decode_tangent_frame(frame);
    let mut uvs = [[0.0; 2]; 4];
    for (set, (uv, range)) in uvs.iter_mut().zip(ranges).enumerate() {
        let at = 4 + set * 4;
        *uv = range.dequantize([
            u16::from_le_bytes([v[at], v[at + 1]]),
            u16::from_le_bytes([v[at + 2], v[at + 3]]),
        ]);
    }
    let at = 4 + ranges.len() * 4;
    let color = (stride >= at + 4).then(|| [v[at], v[at + 1], v[at + 2], v[at + 3]]);
    VertexAttributes {
        normal,
        tangent,
        bitangent_sign: sign,
        uvs,
        color,
    }
}
