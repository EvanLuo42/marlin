//! Clusters for the cluster DAG (0009 §3.1 item 1): triangles split into clusters of up to
//! [`CLUSTER_MAX_TRIANGLES`] triangles over up to [`CLUSTER_MAX_VERTICES`] vertices, grouped by
//! spatial position and normal coherence (the [`meshlet`] builder's growth), with bounding
//! spheres and normal cones for culling.
//!
//! A cluster keeps its triangles over the mesh's vertices: the DAG simplifies by half-edge
//! collapse, which never invents a vertex, so every level of the DAG indexes the same vertices.

use marlin_render::asset::cluster::{CLUSTER_MAX_TRIANGLES, CLUSTER_MAX_VERTICES};

use crate::meshlet;

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Cluster {
    /// Triangle list over mesh vertices.
    pub indices: Vec<u32>,
    /// Mesh section of each triangle.
    pub sections: Vec<u16>,
}

impl Cluster {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// The distinct vertices, in order of first use.
    pub fn vertices(&self) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::with_capacity(self.indices.len());
        for &v in &self.indices {
            if !out.contains(&v) {
                out.push(v);
            }
        }
        out
    }

    /// Culling bounds: a bounding sphere and the normal cone.
    pub fn bounds(&self, positions: &[[f32; 3]]) -> meshlet::Bounds {
        let vertices = self.vertices();
        let local: Vec<u8> = self
            .indices
            .iter()
            .map(|v| vertices.iter().position(|x| x == v).unwrap_or(0) as u8)
            .collect();
        let built = meshlet::Built {
            vertex_offset: 0,
            vertex_count: vertices.len() as u32,
            triangle_offset: 0,
            triangle_count: self.triangle_count() as u32,
        };
        meshlet::bounds(&built, &vertices, &local, positions)
    }
}

/// Splits the triangle list `indices` (with a section per triangle) into clusters.
pub fn split(indices: &[u32], sections: &[u16], positions: &[[f32; 3]]) -> Vec<Cluster> {
    debug_assert_eq!(indices.len() / 3, sections.len());
    // Compact to the vertices used: the builder's tables are per vertex, and higher DAG levels
    // split one small group at a time.
    let mut mesh_of: Vec<u32> = Vec::new();
    let mut local_of: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let local: Vec<u32> = indices
        .iter()
        .map(|&v| {
            *local_of.entry(v).or_insert_with(|| {
                mesh_of.push(v);
                mesh_of.len() as u32 - 1
            })
        })
        .collect();
    let local_positions: Vec<[f32; 3]> = mesh_of.iter().map(|&v| positions[v as usize]).collect();
    // The builder loses triangle identity; find each triangle's section again by its corners.
    let mut section_of: std::collections::HashMap<[u32; 3], Vec<u16>> =
        std::collections::HashMap::new();
    for (t, &s) in local.as_chunks::<3>().0.iter().zip(sections) {
        section_of.entry(*t).or_default().push(s);
    }
    let built = meshlet::build(
        &local,
        &local_positions,
        CLUSTER_MAX_VERTICES,
        CLUSTER_MAX_TRIANGLES,
    );
    built
        .meshlets
        .iter()
        .map(|m| {
            let verts = &built.vertices[m.vertex_offset as usize..][..m.vertex_count as usize];
            let tris =
                &built.triangles[m.triangle_offset as usize..][..m.triangle_count as usize * 3];
            let mut cluster = Cluster::default();
            for t in tris.as_chunks::<3>().0 {
                let corners = [t[0], t[1], t[2]].map(|l| verts[l as usize]);
                let section = section_of
                    .get_mut(&corners)
                    .and_then(Vec::pop)
                    .unwrap_or_default();
                cluster.indices.extend(corners.map(|l| mesh_of[l as usize]));
                cluster.sections.push(section);
            }
            cluster
        })
        .collect()
}
