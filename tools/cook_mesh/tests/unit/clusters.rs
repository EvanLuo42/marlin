//! The cluster DAG's cook side (0009 §3.1, §10 "平台无关"): clustering, the DAG and its LOD
//! cuts, vertex quantization, pages and the group hierarchy, the RT LOD.

use std::collections::HashMap;

use marlin_cook_mesh::cluster;
use marlin_math::Rng;

use crate::support::icosphere;

fn sorted_triangles(indices: &[u32]) -> Vec<[u32; 3]> {
    let mut t: Vec<[u32; 3]> = indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| [t[0], t[1], t[2]])
        .collect();
    t.sort();
    t
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Sections in bands of height, so clusters straddle section borders.
fn banded_sections(positions: &[[f32; 3]], indices: &[u32]) -> Vec<u16> {
    indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let y = (positions[t[0] as usize][1] + positions[t[1] as usize][1]) * 0.5;
            ((y + 1.1) * 1.5) as u16
        })
        .collect()
}

#[test]
fn clusters_cover_every_triangle_within_limits() {
    let s = icosphere(4);
    let sections = banded_sections(&s.positions, &s.indices);
    let clusters = cluster::split(&s.indices, &sections, &s.positions);

    let mut covered: Vec<u32> = Vec::new();
    let mut section_of: HashMap<[u32; 3], u16> = HashMap::new();
    for (t, &sec) in s.indices.as_chunks::<3>().0.iter().zip(&sections) {
        section_of.insert(*t, sec);
    }
    for c in &clusters {
        assert!(c.triangle_count() > 0 && c.triangle_count() <= 128);
        assert!(c.vertices().len() <= 128);
        assert_eq!(c.sections.len(), c.triangle_count());
        for (t, &sec) in c.indices.as_chunks::<3>().0.iter().zip(&c.sections) {
            assert_eq!(section_of[t], sec, "a triangle keeps its section");
        }
        covered.extend_from_slice(&c.indices);
    }
    assert_eq!(
        sorted_triangles(&s.indices),
        sorted_triangles(&covered),
        "every triangle exactly once, winding kept"
    );
    let average = (s.indices.len() / 3) as f32 / clusters.len() as f32;
    assert!(average > 100.0, "average {average} triangles per cluster");

    // Bounds hold every vertex; a culled cone means every triangle faces away.
    let mut rng = Rng::new(7);
    for c in &clusters {
        let b = c.bounds(&s.positions);
        for v in c.vertices() {
            let d = sub(s.positions[v as usize], b.center);
            assert!(dot(d, d).sqrt() <= b.radius);
        }
        let axis = [0, 1, 2].map(|i| b.cone[i] as f32 / 127.0);
        let cutoff = b.cone[3] as f32 / 127.0;
        for _ in 0..16 {
            let camera = [0, 1, 2].map(|_| (rng.f32() - 0.5) * 8.0);
            let d = sub(b.center, camera);
            if dot(d, axis) < cutoff * dot(d, d).sqrt() + b.radius {
                continue;
            }
            for t in c.indices.as_chunks::<3>().0 {
                let [p0, p1, p2] = t.map(|v| s.positions[v as usize]);
                let n = cross(sub(p1, p0), sub(p2, p0));
                assert!(dot(n, sub(camera, p0)) <= 1e-6, "a culled cluster had a front face");
            }
        }
    }
}
