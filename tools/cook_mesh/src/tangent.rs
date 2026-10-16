//! Tangent generation for meshes imported without tangents.
//!
//! Per-corner tangents from UV derivatives (Lengyel, "Computing Tangent Space Basis Vectors for
//! an Arbitrary Mesh", 2001), weighted by corner angle as MikkTSpace weights them, accumulated
//! per vertex, orthonormalized against the normal. Vertices whose triangles disagree on
//! handedness (mirrored UVs meeting at a vertex shared across the mirror) are split, so every
//! triangle gets a consistent frame.
//!
//! glTF's texture space has V pointing down; normal maps follow the OpenGL convention (+Y up in
//! the tangent frame), so derivatives are taken against `1 - v`, as DCC exporters bake them.
//! This is not bit-for-bit MikkTSpace: normal maps baked against MikkTSpace frames match it
//! closely on smooth meshes but not exactly at splits; assets that need exact agreement should
//! export their tangents.

use marlin_render::asset::mesh::basis;

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(a: [f32; 3]) -> Option<[f32; 3]> {
    let l = dot(a, a).sqrt();
    (l > 1e-20).then(|| a.map(|c| c / l))
}

/// The result: a tangent per vertex (xyz, w the bitangent sign), with vertices appended where
/// handedness splits them.
pub struct Tangents {
    pub tangents: Vec<[f32; 4]>,
    /// For each vertex appended past the input's, the input vertex it copies.
    pub splits: Vec<u32>,
    /// The triangle list, pointing split triangles at the copies.
    pub indices: Vec<u32>,
}

/// Generates tangents for `indices` over `positions`, `normals` (unit) and `uvs`.
pub fn generate(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    uvs: Option<&[[f32; 2]]>,
    indices: &[u32],
) -> Tangents {
    let n = positions.len();
    let Some(uvs) = uvs else {
        // No UVs: any frame around the normal.
        return Tangents {
            tangents: normals
                .iter()
                .map(|&nr| {
                    let t = basis(nr).0;
                    [t[0], t[1], t[2], 1.0]
                })
                .collect(),
            splits: Vec::new(),
            indices: indices.to_vec(),
        };
    };
    // Per triangle: tangent, bitangent; per corner: angle weight.
    let triangle_count = indices.len() / 3;
    let mut tri_frame: Vec<Option<([f32; 3], [f32; 3])>> = Vec::with_capacity(triangle_count);
    for t in indices.as_chunks::<3>().0 {
        let [a, b, c] = [t[0], t[1], t[2]].map(|v| v as usize);
        let (e1, e2) = (
            sub(positions[b], positions[a]),
            sub(positions[c], positions[a]),
        );
        let uv = |v: usize| [uvs[v][0], 1.0 - uvs[v][1]];
        let (du1, dv1) = (uv(b)[0] - uv(a)[0], uv(b)[1] - uv(a)[1]);
        let (du2, dv2) = (uv(c)[0] - uv(a)[0], uv(c)[1] - uv(a)[1]);
        let det = du1 * dv2 - du2 * dv1;
        if det.abs() < 1e-20 {
            tri_frame.push(None);
            continue;
        }
        let r = 1.0 / det;
        let tangent = std::array::from_fn(|i| (e1[i] * dv2 - e2[i] * dv1) * r);
        let bitangent = std::array::from_fn(|i| (e2[i] * du1 - e1[i] * du2) * r);
        tri_frame.push(Some((tangent, bitangent)));
    }
    let corner_angle = |t: usize, k: usize| -> f32 {
        let v = |i: usize| positions[indices[t * 3 + (k + i) % 3] as usize];
        match (normalize(sub(v(1), v(0))), normalize(sub(v(2), v(0)))) {
            (Some(x), Some(y)) => dot(x, y).clamp(-1.0, 1.0).acos(),
            _ => 0.0,
        }
    };
    // Handedness of each triangle at each corner's normal.
    let handedness = |t: usize, v: usize| -> f32 {
        match tri_frame[t] {
            Some((tg, bt)) if dot(cross(normals[v], tg), bt) < 0.0 => -1.0,
            _ => 1.0,
        }
    };

    // Majority handedness per vertex, by angle weight.
    let mut votes = vec![0.0f32; n];
    for t in 0..triangle_count {
        if tri_frame[t].is_none() {
            continue;
        }
        for k in 0..3 {
            let v = indices[t * 3 + k] as usize;
            votes[v] += handedness(t, v) * corner_angle(t, k).max(1e-6);
        }
    }
    let mut sign: Vec<f32> = votes
        .iter()
        .map(|&s| if s < 0.0 { -1.0 } else { 1.0 })
        .collect();

    // Split minority-handed corners onto copies (one copy per vertex and sign).
    let mut out_indices = indices.to_vec();
    let mut splits: Vec<u32> = Vec::new();
    let mut copy_of: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for t in 0..triangle_count {
        if tri_frame[t].is_none() {
            continue;
        }
        for k in 0..3 {
            let v = indices[t * 3 + k];
            if handedness(t, v as usize) != sign[v as usize] {
                let copy = *copy_of.entry(v).or_insert_with(|| {
                    splits.push(v);
                    (n + splits.len() - 1) as u32
                });
                out_indices[t * 3 + k] = copy;
            }
        }
    }
    let total = n + splits.len();
    for &v in &splits {
        sign.push(-sign[v as usize]);
    }
    let source = |v: usize| if v < n { v } else { splits[v - n] as usize };

    // Accumulate.
    let mut sum = vec![[0.0f32; 3]; total];
    for t in 0..triangle_count {
        let Some((tg, _)) = tri_frame[t] else {
            continue;
        };
        for k in 0..3 {
            let v = out_indices[t * 3 + k] as usize;
            let w = corner_angle(t, k);
            for i in 0..3 {
                sum[v][i] += tg[i] * w;
            }
        }
    }
    let tangents = (0..total)
        .map(|v| {
            let nr = normals[source(v)];
            let t = sum[v];
            let ortho = sub(t, nr.map(|c| c * dot(nr, t)));
            let t = normalize(ortho).unwrap_or_else(|| basis(nr).0);
            [t[0], t[1], t[2], sign[v]]
        })
        .collect();
    Tangents {
        tangents,
        splits,
        indices: out_indices,
    }
}
