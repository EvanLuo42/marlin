//! The cluster DAG's cook side (0009 §3.1, §10 "平台无关"): clustering, the DAG and its LOD
//! cuts, vertex quantization, pages and the group hierarchy, the RT LOD.

use std::collections::HashMap;

use marlin_cook_mesh::{cluster, dag, quantize};
use marlin_math::Rng;
use marlin_render::asset::cluster::{UvRange, read_cluster_attributes};
use marlin_render::asset::mesh::{dequantize_position, position_scale};

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
                assert!(
                    dot(n, sub(camera, p0)) <= 1e-6,
                    "a culled cluster had a front face"
                );
            }
        }
    }
}

fn dag_of(s: &crate::support::Sphere, sections: &[u16]) -> dag::Dag {
    let attributes: Vec<f32> = s
        .normals
        .iter()
        .zip(&s.uvs)
        .flat_map(|(n, uv)| {
            [
                n[0] * 0.05,
                n[1] * 0.05,
                n[2] * 0.05,
                uv[0] * 0.1,
                uv[1] * 0.1,
            ]
        })
        .collect();
    dag::build(&dag::Input {
        positions: &s.positions,
        attributes: &attributes,
        attribute_count: 5,
        indices: &s.indices,
        sections,
    })
}

/// Position ids: vertices at the same position are the same point of the surface.
fn position_ids(positions: &[[f32; 3]]) -> Vec<u32> {
    let mut ids: HashMap<[u32; 3], u32> = HashMap::new();
    positions
        .iter()
        .map(|p| {
            let next = ids.len() as u32;
            *ids.entry(p.map(f32::to_bits)).or_insert(next)
        })
        .collect()
}

/// Directed edges (by position) of the triangles in `clusters` without a reverse among them:
/// the border of the surface they make.
fn border(dag: &dag::Dag, clusters: &[u32], ids: &[u32]) -> Vec<(u32, u32)> {
    let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
    for &c in clusters {
        for t in dag.clusters[c as usize].cluster.indices.as_chunks::<3>().0 {
            let p = t.map(|v| ids[v as usize]);
            if p[0] == p[1] || p[1] == p[2] || p[0] == p[2] {
                continue;
            }
            for k in 0..3 {
                *edges.entry((p[k], p[(k + 1) % 3])).or_default() += 1;
            }
        }
    }
    let mut out: Vec<(u32, u32)> = edges
        .keys()
        .filter(|&&(a, b)| !edges.contains_key(&(b, a)))
        .copied()
        .collect();
    out.sort_unstable();
    out
}

/// Every edge of the cut's surface used once in each direction: closed, no cracks, no overlaps.
fn assert_watertight(dag: &dag::Dag, cut: &[u32], ids: &[u32], what: &str) {
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    for &c in cut {
        for t in dag.clusters[c as usize].cluster.indices.as_chunks::<3>().0 {
            let p = t.map(|v| ids[v as usize]);
            if p[0] == p[1] || p[1] == p[2] || p[0] == p[2] {
                continue;
            }
            for k in 0..3 {
                *edges.entry((p[k], p[(k + 1) % 3])).or_default() += 1;
            }
        }
    }
    assert!(!edges.is_empty(), "{what}: an empty cut");
    for (&(a, b), &n) in &edges {
        assert_eq!(n, 1, "{what}: edge {a}-{b} used {n} times");
        assert_eq!(
            edges.get(&(b, a)),
            Some(&1),
            "{what}: crack at edge {a}-{b}"
        );
    }
}

fn inside(inner: [f32; 4], outer: [f32; 4]) -> bool {
    let d = sub(
        [inner[0], inner[1], inner[2]],
        [outer[0], outer[1], outer[2]],
    );
    dot(d, d).sqrt() + inner[3] <= outer[3] * (1.0 + 1e-5)
}

fn triangles(dag: &dag::Dag, clusters: &[u32]) -> usize {
    clusters
        .iter()
        .map(|&c| dag.clusters[c as usize].cluster.triangle_count())
        .sum()
}

#[test]
fn dag_errors_are_monotonic_and_converge_to_one_root() {
    let s = icosphere(5);
    let sections = vec![0u16; s.indices.len() / 3];
    let dag = dag_of(&s, &sections);
    assert!(dag.levels >= 6, "{} levels", dag.levels);
    let roots: Vec<&dag::Group> = dag.groups.iter().filter(|g| g.is_root()).collect();
    assert_eq!(
        roots.len(),
        1,
        "a closed sphere simplifies down to one root group"
    );
    assert_eq!(roots[0].clusters.len(), 1);
    for (i, c) in dag.clusters.iter().enumerate() {
        let g = &dag.groups[c.group as usize];
        assert!(g.clusters.contains(&(i as u32)));
        assert!(
            dag.parent_error(i) >= c.lod_error,
            "cluster {i}: parent error below its own"
        );
        assert!(
            inside(c.lod_sphere, g.sphere),
            "cluster {i}: LOD sphere outside its group's"
        );
        if c.level == 0 {
            assert_eq!(c.lod_error, 0.0);
        }
    }
    for g in &dag.groups {
        assert!(g.clusters.len() <= dag::MAX_GROUP);
        if g.clusters.len() < 8 {
            // Only where few clusters are left.
            let at_level = dag.clusters.iter().filter(|c| c.level == g.level).count();
            assert!(
                at_level < 16,
                "a group of {} at level {}",
                g.clusters.len(),
                g.level
            );
        }
        for &c in &g.simplified {
            let c = &dag.clusters[c as usize];
            assert_eq!((c.lod_error, c.lod_sphere), (g.error, g.sphere));
            assert_eq!(c.level, g.level + 1);
        }
        if !g.is_root() {
            let (before, after) = (triangles(&dag, &g.clusters), triangles(&dag, &g.simplified));
            assert!(after * 10 <= before * 6, "{before} -> {after} triangles");
        }
    }
    // The bumps cost error; a coarse level costs more than a fine one.
    let error_at = |level: u32| {
        dag.groups
            .iter()
            .filter(|g| g.level == level && !g.is_root())
            .map(|g| g.error)
            .fold(0.0f32, f32::max)
    };
    assert!(error_at(0) > 0.0 && error_at(3) > error_at(0));
}

#[test]
fn group_borders_are_locked() {
    let s = icosphere(5);
    let sections = banded_sections(&s.positions, &s.indices);
    let dag = dag_of(&s, &sections);
    let ids = position_ids(&s.positions);
    let mut simplified = 0;
    for g in dag.groups.iter().filter(|g| !g.is_root()) {
        assert_eq!(
            border(&dag, &g.clusters, &ids),
            border(&dag, &g.simplified, &ids),
            "a group's border moved when it was simplified"
        );
        simplified += 1;
    }
    assert!(simplified > 10);
}

#[test]
fn every_lod_cut_is_watertight() {
    let s = icosphere(5);
    let sections = banded_sections(&s.positions, &s.indices);
    let dag = dag_of(&s, &sections);
    let ids = position_ids(&s.positions);
    let max_error = dag
        .groups
        .iter()
        .filter(|g| !g.is_root())
        .map(|g| g.error)
        .fold(0.0f32, f32::max);
    let mut rng = Rng::new(11);

    // Object-space thresholds, from the full mesh to the root.
    assert_eq!(
        triangles(&dag, &dag.cut(0.0)),
        s.indices.len() / 3,
        "threshold 0 is the source mesh"
    );
    let mut counts = Vec::new();
    for _ in 0..24 {
        let threshold = rng.f32() * max_error * 1.2;
        let cut = dag.cut(threshold);
        assert_watertight(&dag, &cut, &ids, &format!("threshold {threshold}"));
        counts.push((threshold, triangles(&dag, &cut)));
    }
    counts.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert!(
        counts.windows(2).all(|w| w[0].1 >= w[1].1),
        "coarser thresholds, fewer triangles"
    );

    // View-dependent: the error projected from a camera, as the GPU will select.
    let project = |error: f32, s: [f32; 4], camera: [f32; 3]| {
        let d = sub([s[0], s[1], s[2]], camera);
        error / (dot(d, d).sqrt() - s[3]).max(1e-4)
    };
    for _ in 0..24 {
        let camera = [0, 1, 2].map(|_| (rng.f32() - 0.5) * 12.0);
        let threshold = rng.f32() * 0.02;
        let cut: Vec<u32> = (0..dag.clusters.len())
            .filter(|&c| {
                let cl = &dag.clusters[c];
                let g = &dag.groups[cl.group as usize];
                project(cl.lod_error, cl.lod_sphere, camera) <= threshold
                    && project(g.error, g.sphere, camera) > threshold
            })
            .map(|c| c as u32)
            .collect();
        assert_watertight(
            &dag,
            &cut,
            &ids,
            &format!("camera {camera:?}, threshold {threshold}"),
        );
    }
}

fn normalize(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt();
    a.map(|c| c / l)
}

fn angle(a: [f32; 3], b: [f32; 3]) -> f32 {
    dot(normalize(a), normalize(b))
        .clamp(-1.0, 1.0)
        .acos()
        .to_degrees()
}

#[test]
fn quantization_round_trip() {
    let s = icosphere(4);
    let mut rng = Rng::new(5);
    let n = s.positions.len();
    // Tangents orthogonal to the normals, random signs; a tiling UV set and a small one.
    let tangents: Vec<[f32; 4]> = s
        .normals
        .iter()
        .map(|&nrm| {
            let r = [0, 1, 2].map(|_| rng.f32() - 0.5);
            let t = normalize(cross(nrm, r));
            [t[0], t[1], t[2], if rng.f32() < 0.5 { -1.0 } else { 1.0 }]
        })
        .collect();
    let uvs = vec![
        s.uvs
            .iter()
            .map(|uv| [uv[0] * 40.0 - 7.0, uv[1] * 25.0])
            .collect::<Vec<_>>(),
        s.uvs.clone(),
    ];
    let colors: Vec<[f32; 4]> = (0..n).map(|_| [0, 1, 2, 3].map(|_| rng.f32())).collect();
    let skin: Vec<([u16; 4], [u8; 4])> = (0..n as u16)
        .map(|i| ([i, i + 1, 0, 3], [200, 55, 0, 0]))
        .collect();
    let v = quantize::Vertices {
        positions: &s.positions,
        normals: &s.normals,
        tangents: &tangents,
        uvs: &uvs,
        colors: Some(&colors),
        skin: Some(&skin),
    };
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in &s.positions {
        for i in 0..3 {
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    }
    let scale = position_scale(lo, hi);
    let stride = quantize::attribute_stride(&v);
    let mesh_ranges: Vec<UvRange> = uvs
        .iter()
        .map(|set| UvRange::of(set.iter().copied()))
        .collect();
    assert_eq!(stride, 4 + 8 + 4);

    let clusters = cluster::split(&s.indices, &vec![0; s.indices.len() / 3], &s.positions);
    let mut decoded_at: HashMap<u32, [u16; 3]> = HashMap::new();
    for c in &clusters {
        let vertices = c.vertices();
        for positions in [
            quantize::Positions::Quantized { min: lo, scale },
            quantize::Positions::Float,
        ] {
            let e = quantize::encode(&vertices, &v, positions);
            assert_eq!(e.positions.len(), vertices.len() * positions.stride());
            assert_eq!(e.attributes.len(), vertices.len() * stride);
            assert_eq!(e.skin.len(), vertices.len() * 12);
            for (k, &i) in vertices.iter().enumerate() {
                let i = i as usize;
                let p = &e.positions[k * positions.stride()..];
                match positions {
                    quantize::Positions::Quantized { .. } => {
                        let q = [0, 1, 2].map(|a| u16::from_le_bytes([p[a * 2], p[a * 2 + 1]]));
                        let back = dequantize_position(q, lo, scale);
                        for a in 0..3 {
                            assert!((back[a] - s.positions[i][a]).abs() <= scale[a] * 0.5 + 1e-6);
                        }
                        // The same position decodes the same in every cluster.
                        assert_eq!(*decoded_at.entry(i as u32).or_insert(q), q);
                    }
                    quantize::Positions::Float => {
                        let back = [0, 1, 2]
                            .map(|a| f32::from_le_bytes(p[a * 4..a * 4 + 4].try_into().unwrap()));
                        assert_eq!(back, s.positions[i]);
                    }
                }
                let a = read_cluster_attributes(&e.attributes, stride, k, &e.uv_ranges);
                assert!(
                    angle(a.normal, s.normals[i]) < 0.25,
                    "normal off by {}°",
                    angle(a.normal, s.normals[i])
                );
                let t = tangents[i];
                assert!(angle(a.tangent, [t[0], t[1], t[2]]) < 0.5);
                assert_eq!(a.bitangent_sign, t[3]);
                for (set, range) in e.uv_ranges.iter().enumerate() {
                    for c in 0..2 {
                        let error = (a.uvs[set][c] - uvs[set][i][c]).abs();
                        // Half a step of the cluster's range, plus float rounding of the decode.
                        assert!(
                            error <= range.scale[c] * 0.5 + uvs[set][i][c].abs() * 1e-6 + 1e-7,
                            "UV off by {error}"
                        );
                        // Never coarser than one range over the whole mesh.
                        assert!(range.scale[c] <= mesh_ranges[set].scale[c]);
                    }
                }
                let color = a.color.unwrap();
                for c in 0..4 {
                    assert!((color[c] as f32 / 255.0 - colors[i][c]).abs() <= 0.5 / 255.0 + 1e-6);
                }
                assert_eq!(&e.skin[k * 12..k * 12 + 2], &skin[i].0[0].to_le_bytes());
                assert_eq!(&e.skin[k * 12 + 8..k * 12 + 12], &skin[i].1);
            }
        }
    }
}
