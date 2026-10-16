//! Mesh data as the renderer consumes it.

use alloc::vec::Vec;

use marlin_math::Vec3;

/// One vertex, as `StructuredBuffer<Vertex>` reads it (24 bytes, no padding).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
}

/// Triangle-list geometry: counter-clockwise front faces, 32-bit indices.
#[derive(Clone, Copy, Debug)]
pub struct MeshData<'a> {
    pub vertices: &'a [Vertex],
    pub indices: &'a [u32],
}

/// A unit cube centered on the origin with per-face normals: 24 vertices, 36 indices.
pub fn cube() -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(24);
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
        let base = vertices.len() as u32;
        for (a, b) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let p = (normal + u * a + v * b) * 0.5;
            vertices.push(Vertex {
                position: [p.x, p.y, p.z],
                normal: [normal.x, normal.y, normal.z],
            });
        }
        indices.extend([0, 1, 2, 0, 2, 3].map(|i| base + i));
    }
    (vertices, indices)
}
