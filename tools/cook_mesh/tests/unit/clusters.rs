//! The cluster DAG's cook side (0009 §3.1, §10 "平台无关"): clustering, the DAG and its LOD
//! cuts, vertex quantization, pages and the group hierarchy, the RT LOD.

use std::collections::HashMap;

use marlin_cook_mesh::{cluster, dag, page, quantize, rt};
use marlin_math::Rng;
use marlin_render::asset::cluster::{
    HIERARCHY_LEAF, HIERARCHY_NONE, HierarchyChild, PAGE_SIZE, PageLayout, PageView, UvRange,
    read_cluster_attributes,
};
use marlin_render::asset::mesh::{dequantize_position, position_scale, quantize_position};

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

/// Vertex streams for `s` with everything a page can hold: two UV sets, colors and skin.
struct Streams {
    tangents: Vec<[f32; 4]>,
    uvs: Vec<Vec<[f32; 2]>>,
    colors: Vec<[f32; 4]>,
    skin: Vec<([u16; 4], [u8; 4])>,
}

fn streams(s: &crate::support::Sphere) -> Streams {
    Streams {
        tangents: s
            .normals
            .iter()
            .map(|&n| {
                let t = normalize(cross(n, [0.3, 1.0, 0.2]));
                [t[0], t[1], t[2], 1.0]
            })
            .collect(),
        uvs: vec![
            s.uvs.clone(),
            s.uvs.iter().map(|uv| [uv[1] * 3.0, uv[0]]).collect(),
        ],
        colors: s
            .positions
            .iter()
            .map(|p| [p[0].abs(), p[1].abs(), p[2].abs(), 1.0])
            .collect(),
        skin: (0..s.positions.len())
            .map(|i| ([(i % 7) as u16, 0, 0, 0], [255, 0, 0, 0]))
            .collect(),
    }
}

fn mesh_bounds(positions: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in positions {
        for i in 0..3 {
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    }
    (lo, hi)
}

#[test]
fn pages_hold_every_cluster_within_limits() {
    let s = icosphere(5);
    let sections = banded_sections(&s.positions, &s.indices);
    let dag = dag_of(&s, &sections);
    let st = streams(&s);
    let v = quantize::Vertices {
        positions: &s.positions,
        normals: &s.normals,
        tangents: &st.tangents,
        uvs: &st.uvs,
        colors: Some(&st.colors),
        skin: Some(&st.skin),
    };
    let (lo, hi) = mesh_bounds(&s.positions);
    let scale = position_scale(lo, hi);
    let packed = page::pack(&dag, &v, quantize::Positions::Quantized { min: lo, scale });
    let layout = PageLayout {
        uv_sets: 2,
        position_stride: 8,
        attribute_stride: quantize::attribute_stride(&v) as u32,
        skin_stride: 12,
        mesh_sections: sections.iter().copied().max().unwrap() as u32 + 1,
    };
    assert!(packed.pages.len() >= 4, "{} pages", packed.pages.len());
    assert!(packed.root_page_count >= 1 && packed.root_page_count < packed.pages.len() as u32);

    // Limits; every cluster in exactly one place, decoding to its triangles and sections.
    let views: Vec<PageView<'_>> = packed
        .pages
        .iter()
        .map(|p| PageView::new(p, layout).expect("a valid page"))
        .collect();
    let mut seen = vec![false; dag.clusters.len()];
    let mut fill = 0usize;
    for (i, (view, record)) in views.iter().zip(&packed.page_records).enumerate() {
        assert!(view.bytes.len() <= PAGE_SIZE);
        assert_eq!(record.cluster_count, view.header.cluster_count);
        fill += view.bytes.len();
        for k in 0..record.cluster_count as usize {
            let r = view.cluster(k);
            let c = (0..dag.clusters.len())
                .find(|&c| packed.cluster_location[c] == (i as u32, k as u32))
                .expect("a cluster of the DAG");
            assert!(!seen[c]);
            seen[c] = true;
            let dc = &dag.clusters[c];
            assert_eq!(
                (r.group, r.source_group, r.lod_error),
                (dc.group, dc.source_group, dc.lod_error)
            );
            assert_eq!(r.triangle_count as usize, dc.cluster.triangle_count());
            for t in 0..r.triangle_count as usize {
                let tri = view.triangle(r.triangle_offset as usize + t);
                for corner in 0..3 {
                    let decoded =
                        view.position(r.vertex_offset as usize + tri[corner] as usize, lo, scale);
                    let source = s.positions[dc.cluster.indices[t * 3 + corner] as usize];
                    let expected =
                        dequantize_position(quantize_position(source, lo, scale), lo, scale);
                    assert_eq!(decoded, expected);
                }
                assert_eq!(
                    view.section(r.section_offset as usize + tri[3] as usize),
                    dc.cluster.sections[t],
                    "a triangle keeps its section"
                );
            }
        }
    }
    assert!(seen.iter().all(|&s| s));
    // Pages fill: all but the last root page and the last page at least 3/4 full.
    let average = fill as f32 / packed.pages.len() as f32;
    assert!(
        average > PAGE_SIZE as f32 * 0.75,
        "average page {average} bytes"
    );

    // Parts cover each group's clusters; dependencies point back, roots come first.
    for (g, group) in packed.groups.iter().enumerate() {
        let parts = &packed.parts[group.first_part as usize..][..group.part_count as usize];
        let mut clusters: Vec<u32> = Vec::new();
        for p in parts {
            assert_eq!(p.group, g as u32);
            for k in p.first_cluster..p.first_cluster + p.cluster_count {
                let c = (0..dag.clusters.len())
                    .find(|&c| packed.cluster_location[c] == (p.page, k))
                    .unwrap();
                clusters.push(c as u32);
            }
            if dag.groups[g].is_root() {
                assert!(p.page < packed.root_page_count);
            }
        }
        let mut expected = dag.groups[g].clusters.clone();
        expected.sort_unstable();
        clusters.sort_unstable();
        assert_eq!(clusters, expected);
        assert_eq!(group.error, dag.groups[g].error);
    }
    let mut splits = 0;
    for (i, record) in packed.page_records.iter().enumerate() {
        let deps = &packed.dependencies[record.first_dependency as usize..]
            [..record.dependency_count as usize];
        let groups_here: Vec<u32> = (0..record.cluster_count as usize)
            .map(|k| views[i].cluster(k).group)
            .collect();
        let mut split = false;
        for &d in deps {
            assert!(d < packed.pages.len() as u32 && d != i as u32);
            // A later page only for another part of a group split across pages.
            if d > i as u32 {
                split = true;
                assert!(
                    packed
                        .parts
                        .iter()
                        .any(|p| p.page == d && groups_here.contains(&p.group))
                );
            }
        }
        splits += usize::from(split);
        // The coarser clusters of this page's groups are on pages it depends on.
        for k in 0..record.cluster_count as usize {
            let g = views[i].cluster(k).group as usize;
            for &c in &dag.groups[g].simplified {
                let page = packed.cluster_location[c as usize].0;
                assert!(page == i as u32 || deps.contains(&page));
                assert!(page <= i as u32, "coarser clusters on later pages");
            }
        }
    }
    assert!(splits > 0, "groups split across pages");

    // The hierarchy: every part reached once, spheres and errors enclose what is below.
    let mut reached = vec![0u32; packed.parts.len()];
    let mut stack = vec![0u32];
    let mut nodes_seen = 0;
    // (sphere, max error) of everything below each child, checked against the child.
    fn below(packed: &page::Packed, child: u32, out: &mut Vec<([f32; 4], f32)>) {
        if child & HIERARCHY_LEAF != 0 {
            let p = packed.parts[(child & !HIERARCHY_LEAF) as usize];
            let g = packed.groups[p.group as usize];
            out.push((g.sphere, g.error));
        } else {
            for c in &packed.nodes[child as usize].children {
                if c.child != HIERARCHY_NONE {
                    below(packed, c.child, out);
                }
            }
        }
    }
    while let Some(n) = stack.pop() {
        nodes_seen += 1;
        let node = &packed.nodes[n as usize];
        let used: Vec<&HierarchyChild> = node
            .children
            .iter()
            .filter(|c| c.child != HIERARCHY_NONE)
            .collect();
        assert!(!used.is_empty());
        if n != 0 {
            assert!(used.len() >= 2, "a node with one child");
        }
        for c in used {
            let mut under = Vec::new();
            below(&packed, c.child, &mut under);
            for (sphere, error) in under {
                assert!(
                    inside(sphere, c.sphere),
                    "a group outside its node's sphere"
                );
                assert!(c.max_error >= error);
            }
            if c.child & HIERARCHY_LEAF != 0 {
                reached[(c.child & !HIERARCHY_LEAF) as usize] += 1;
            } else {
                stack.push(c.child);
            }
        }
    }
    assert_eq!(nodes_seen, packed.nodes.len());
    assert!(
        reached.iter().all(|&r| r == 1),
        "every part reached exactly once"
    );

    // A cut decoded from the pages alone is watertight.
    let mut rng = Rng::new(3);
    for _ in 0..8 {
        let threshold = rng.f32() * 0.02;
        let mut edges: HashMap<([u32; 3], [u32; 3]), u32> = HashMap::new();
        for view in &views {
            for k in 0..view.header.cluster_count as usize {
                let r = view.cluster(k);
                if !(r.lod_error <= threshold && packed.groups[r.group as usize].error > threshold)
                {
                    continue;
                }
                for t in 0..r.triangle_count as usize {
                    let tri = view.triangle(r.triangle_offset as usize + t);
                    let p = [0, 1, 2].map(|i| {
                        view.position(r.vertex_offset as usize + tri[i] as usize, lo, scale)
                            .map(f32::to_bits)
                    });
                    if p[0] == p[1] || p[1] == p[2] || p[0] == p[2] {
                        continue;
                    }
                    for i in 0..3 {
                        *edges.entry((p[i], p[(i + 1) % 3])).or_default() += 1;
                    }
                }
            }
        }
        assert!(!edges.is_empty());
        for (&(a, b), &n) in &edges {
            assert_eq!(n, 1);
            assert_eq!(
                edges.get(&(b, a)),
                Some(&1),
                "crack in the decoded cut at {threshold}"
            );
        }
    }
}

#[test]
fn damaged_pages_are_refused() {
    let s = icosphere(3);
    let sections = vec![0u16; s.indices.len() / 3];
    let dag = dag_of(&s, &sections);
    let tangents: Vec<[f32; 4]> = vec![[1.0, 0.0, 0.0, 1.0]; s.positions.len()];
    let uvs = vec![s.uvs.clone()];
    let v = quantize::Vertices {
        positions: &s.positions,
        normals: &s.normals,
        tangents: &tangents,
        uvs: &uvs,
        colors: None,
        skin: None,
    };
    let packed = page::pack(&dag, &v, quantize::Positions::Float);
    let layout = PageLayout {
        uv_sets: 1,
        position_stride: 12,
        attribute_stride: 8,
        skin_stride: 0,
        mesh_sections: 1,
    };
    let page = &packed.pages[0];
    let view = PageView::new(page, layout).unwrap();
    assert!(
        PageView::new(
            page,
            PageLayout {
                mesh_sections: 0,
                ..layout
            }
        )
        .is_err()
    );
    assert!(
        PageView::new(
            page,
            PageLayout {
                uv_sets: 2,
                ..layout
            }
        )
        .is_err()
    );
    assert!(PageView::new(&page[..page.len() - 16], layout).is_err());
    assert!(PageView::new(&page[..8], layout).is_err());
    // A vertex index past its cluster.
    let r = view.cluster(0);
    let mut bad = page.clone();
    bad[view.header.triangles.offset as usize + r.triangle_offset as usize * 4] = r.vertex_count;
    assert!(PageView::new(&bad, layout).is_err());
    // A section entry past the cluster's table.
    let mut bad = page.clone();
    bad[view.header.triangles.offset as usize + 3] = r.section_count;
    assert!(PageView::new(&bad, layout).is_err());
    // Random bytes never panic.
    let mut rng = Rng::new(9);
    for _ in 0..200 {
        let mut bad = page.clone();
        for _ in 0..4 {
            let at = (rng.f32() * bad.len() as f32) as usize % bad.len();
            bad[at] = (rng.f32() * 256.0) as u8;
        }
        let _ = PageView::new(&bad, layout);
    }
}

#[test]
fn the_rt_lod_is_a_watertight_quarter() {
    let s = icosphere(5);
    let sections = banded_sections(&s.positions, &s.indices);
    let section_count = sections.iter().copied().max().unwrap() as usize + 1;
    let dag = dag_of(&s, &sections);
    let source = s.indices.len() / 3;
    let lod = rt::build(&dag, source, rt::DEFAULT_FRACTION, section_count);
    let tris = lod.indices.len() / 3;
    assert!(
        (source / 6..=source / 3).contains(&tris),
        "{tris} of {source} triangles"
    );
    assert!(lod.error > 0.0);

    // Sections in order, covering every index, each triangle in its source section.
    assert_eq!(lod.sections.len(), section_count);
    let mut next = 0;
    for &(first, count) in &lod.sections {
        assert_eq!(first, next);
        assert_eq!(count % 3, 0);
        next += count;
    }
    assert_eq!(next as usize, lod.indices.len());
    let mut section_of: HashMap<[u32; 3], u16> = HashMap::new();
    for (t, &sec) in s.indices.as_chunks::<3>().0.iter().zip(&sections) {
        section_of.insert(*t, sec);
    }
    for c in dag.cut(lod.error) {
        let cl = &dag.clusters[c as usize].cluster;
        for (t, &sec) in cl.indices.as_chunks::<3>().0.iter().zip(&cl.sections) {
            if let Some(&source_section) = section_of.get(t) {
                assert_eq!(source_section, sec);
            }
        }
    }

    // Watertight, over the mesh's positions.
    let ids = position_ids(&s.positions);
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    for t in lod.indices.as_chunks::<3>().0 {
        let p = t.map(|l| ids[lod.vertices[l as usize] as usize]);
        if p[0] == p[1] || p[1] == p[2] || p[0] == p[2] {
            continue;
        }
        for k in 0..3 {
            *edges.entry((p[k], p[(k + 1) % 3])).or_default() += 1;
        }
    }
    for (&(a, b), &n) in &edges {
        assert_eq!(n, 1);
        assert_eq!(edges.get(&(b, a)), Some(&1));
    }

    // The fraction steers it.
    let half = rt::build(&dag, source, 0.5, section_count);
    assert!(half.indices.len() > lod.indices.len());
    let full = rt::build(&dag, source, 1.0, section_count);
    assert_eq!(full.indices.len() / 3, source);
}
