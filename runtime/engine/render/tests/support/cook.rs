//! Test meshes, cooked by the real mesh cooker (`marlin-cook-mesh`), so the renderer sees the
//! format the engine ships.

use marlin_cook_mesh::import::Geometry;
use marlin_math::Vec3;
use marlin_render::asset::Mesh;
use marlin_render::asset::mesh::{MeshView, basis};

/// A cooked mesh: its payload and bulk blobs.
#[derive(Clone)]
pub struct Cooked {
    pub payload: Vec<u8>,
    pub pages: Vec<Vec<u8>>,
}

impl Cooked {
    pub fn view(&self) -> MeshView<'_> {
        let sizes: Vec<u64> = self.pages.iter().map(|p| p.len() as u64).collect();
        MeshView::new(&self.payload, &sizes).unwrap()
    }

    pub fn pages(&self) -> Vec<&[u8]> {
        self.pages.iter().map(Vec::as_slice).collect()
    }
}

/// Cooks a triangle list (counter-clockwise front faces) with `settings`' precision.
/// Tangents follow the normals' basis; no UVs, colors or skin.
pub fn cook_with(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    indices: &[u32],
    settings: Mesh,
) -> Cooked {
    let geometry = Geometry {
        positions: positions.to_vec(),
        normals: normals.to_vec(),
        tangents: normals
            .iter()
            .map(|&n| {
                let t = basis(n).0;
                [t[0], t[1], t[2], 1.0]
            })
            .collect(),
        uvs: Vec::new(),
        colors: None,
        influences: None,
        skin: Vec::new(),
        section_names: vec!["Default".into()],
        triangle_sections: vec![0; indices.len() / 3],
        indices: indices.to_vec(),
    };
    let built = marlin_cook_mesh::build(
        &geometry,
        &Mesh {
            max_uv_sets: 0,
            vertex_colors: false,
            ..settings
        },
    )
    .unwrap();
    Cooked {
        payload: built.payload,
        pages: built.pages,
    }
}

pub fn cook(positions: &[[f32; 3]], normals: &[[f32; 3]], indices: &[u32]) -> Cooked {
    cook_with(positions, normals, indices, Mesh::default())
}

/// A unit cube centered on the origin with per-face normals: 24 vertices, 36 indices.
pub fn cube() -> Cooked {
    let mut positions = Vec::with_capacity(24);
    let mut normals = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    let faces = [
        (Vec3::X, Vec3::Y),
        (-Vec3::X, Vec3::Y),
        (Vec3::Y, -Vec3::Z),
        (-Vec3::Y, Vec3::Z),
        (Vec3::Z, Vec3::Y),
        (-Vec3::Z, Vec3::Y),
    ];
    for (normal, v) in faces {
        // u × v = normal, so u right and v up face the viewer outside: counter-clockwise.
        let u = v.cross(normal);
        let base = positions.len() as u32;
        for (a, b) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let p = (normal + u * a + v * b) * 0.5;
            positions.push([p.x, p.y, p.z]);
            normals.push([normal.x, normal.y, normal.z]);
        }
        indices.extend([0, 1, 2, 0, 2, 3].map(|i| base + i));
    }
    cook(&positions, &normals, &indices)
}

/// A UV sphere of radius 0.5 with smooth normals, cooked: `rings` × `segments` quads, enough
/// triangles for a LOD chain.
#[allow(dead_code, reason = "only the gpu binary draws spheres")]
pub fn sphere(rings: u32, segments: u32) -> Cooked {
    let (positions, normals, indices) = sphere_source(rings, segments);
    cook(&positions, &normals, &indices)
}

/// The sphere's source triangles: positions, normals, indices.
#[allow(dead_code, reason = "only the gpu binary draws spheres")]
pub fn sphere_source(rings: u32, segments: u32) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    for r in 0..=rings {
        let theta = core::f32::consts::PI * r as f32 / rings as f32;
        for s in 0..=segments {
            let phi = core::f32::consts::TAU * s as f32 / segments as f32;
            let n = [
                theta.sin() * phi.cos(),
                theta.cos(),
                theta.sin() * phi.sin(),
            ];
            positions.push(n.map(|c| c * 0.5));
            normals.push(n);
        }
    }
    let mut indices = Vec::new();
    let row = segments + 1;
    for r in 0..rings {
        for s in 0..segments {
            let (a, b) = (r * row + s, (r + 1) * row + s);
            // Seen from outside: counter-clockwise.
            indices.extend([a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    (positions, normals, indices)
}
