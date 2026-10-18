//! Vertex quantization for cluster pages (0009 §3.1 item 5), in the layout of
//! `marlin_render::asset::cluster`: positions over the mesh bounds, octahedral normal and
//! tangent angle in 32 bits, UVs as unorm16 over each cluster's range, RGBA8 color, skin as the
//! mesh's.

use marlin_render::asset::cluster::UvRange;
use marlin_render::asset::mesh::{encode_tangent_frame, quantize_position};

/// The welded vertices of a mesh, one entry per vertex in each stream.
pub struct Vertices<'a> {
    pub positions: &'a [[f32; 3]],
    pub normals: &'a [[f32; 3]],
    /// xyz and the bitangent sign.
    pub tangents: &'a [[f32; 4]],
    /// Up to four sets.
    pub uvs: &'a [Vec<[f32; 2]>],
    /// Linear RGBA in [0, 1].
    pub colors: Option<&'a [[f32; 4]]>,
    /// Joints and unorm8 weights summing to 255.
    pub skin: Option<&'a [([u16; 4], [u8; 4])]>,
}

/// How positions are stored.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Positions {
    /// 3×u16 over the mesh bounds (`min`, decode scale), plus u16 padding.
    Quantized { min: [f32; 3], scale: [f32; 3] },
    /// 3×f32.
    Float,
}

impl Positions {
    pub fn stride(&self) -> usize {
        match self {
            Positions::Quantized { .. } => 8,
            Positions::Float => 12,
        }
    }
}

/// One cluster's vertex streams.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Encoded {
    pub positions: Vec<u8>,
    pub attributes: Vec<u8>,
    /// Empty without skin.
    pub skin: Vec<u8>,
    /// One per UV set.
    pub uv_ranges: Vec<UvRange>,
}

/// Bytes per vertex of the attribute stream.
pub fn attribute_stride(v: &Vertices<'_>) -> usize {
    4 + 4 * v.uvs.len() + if v.colors.is_some() { 4 } else { 0 }
}

/// Encodes the vertices `vertices` (indices into `v`) of one cluster.
pub fn encode(vertices: &[u32], v: &Vertices<'_>, positions: Positions) -> Encoded {
    let mut out = Encoded {
        uv_ranges: v
            .uvs
            .iter()
            .map(|set| UvRange::of(vertices.iter().map(|&i| set[i as usize])))
            .collect(),
        ..Encoded::default()
    };
    for &i in vertices {
        let i = i as usize;
        let p = v.positions[i];
        match positions {
            Positions::Quantized { min, scale } => {
                let q = quantize_position(p, min, scale);
                out.positions.extend(q.iter().flat_map(|c| c.to_le_bytes()));
                out.positions.extend_from_slice(&[0, 0]);
            }
            Positions::Float => out.positions.extend(p.iter().flat_map(|c| c.to_le_bytes())),
        }
        let t = v.tangents[i];
        out.attributes
            .extend(encode_tangent_frame(v.normals[i], [t[0], t[1], t[2]], t[3]).to_le_bytes());
        for (set, range) in v.uvs.iter().zip(&out.uv_ranges) {
            out.attributes
                .extend(range.quantize(set[i]).iter().flat_map(|c| c.to_le_bytes()));
        }
        if let Some(colors) = v.colors {
            out.attributes
                .extend(colors[i].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8));
        }
        if let Some(skin) = v.skin {
            let (joints, weights) = skin[i];
            out.skin.extend(joints.iter().flat_map(|j| j.to_le_bytes()));
            out.skin.extend_from_slice(&weights);
        }
    }
    out
}
