//! The mesh algorithms on synthetic geometry: properties, not exact outputs.

use std::collections::HashSet;

use marlin_cook_mesh::{meshlet, optimize, simplify, tangent};
use marlin_math::Rng;
use marlin_math::scalar::abs_diff_eq;

use crate::support::{grid, sphere};

fn attributes(normals: &[[f32; 3]], uvs: &[[f32; 2]]) -> Vec<f32> {
    normals
        .iter()
        .zip(uvs)
        .flat_map(|(n, uv)| {
            [
                n[0] * 0.05,
                n[1] * 0.05,
                n[2] * 0.05,
                uv[0] * 0.1,
                uv[1] * 0.1,
            ]
        })
        .collect()
}

#[test]
fn a_flat_grid_collapses_but_keeps_its_border() {
    let (positions, uvs, indices) = grid(32);
    let normals = vec![[0.0, 1.0, 0.0]; positions.len()];
    let sections = vec![0u16; indices.len() / 3];
    let out = simplify::simplify(
        &simplify::Input {
            positions: &positions,
            attributes: &attributes(&normals, &uvs),
            attribute_count: 5,
            indices: &indices,
            sections: &sections,
        },
        64,
        f32::MAX,
    );
    let tris = out.indices.len() / 3;
    assert!(tris < 2048 / 4, "simplified a lot: {tris}");
    assert!(out.error < 1e-5, "a plane stays a plane: {}", out.error);
    // Every border vertex survives (locked borders keep neighboring LODs crack-free).
    let used: HashSet<u32> = out.indices.iter().copied().collect();
    for (v, p) in positions.iter().enumerate() {
        let border = p[0] == 0.0 || p[0] == 1.0 || p[2] == 0.0 || p[2] == 1.0;
        if border {
            assert!(
                used.contains(&(v as u32)),
                "border vertex {v} at {p:?} was removed"
            );
        }
    }
    // Still facing up, nothing flipped.
    for t in out.indices.as_chunks::<3>().0 {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| positions[i as usize]);
        let n = (c[0] - a[0]) * (b[2] - a[2]) - (b[0] - a[0]) * (c[2] - a[2]);
        assert!(n > 0.0, "flipped triangle");
    }
}

#[test]
fn a_sphere_simplifies_within_its_error_and_keeps_the_seam() {
    let s = sphere(32, 64);
    let sections = vec![0u16; s.indices.len() / 3];
    let input = simplify::Input {
        positions: &s.positions,
        attributes: &attributes(&s.normals, &s.uvs),
        attribute_count: 5,
        indices: &s.indices,
        sections: &sections,
    };
    let before = s.indices.len() / 3;
    let out = simplify::simplify(&input, before / 4, f32::MAX);
    let after = out.indices.len() / 3;
    assert!(after <= before / 4 + before / 20, "{before} → {after}");
    assert!(out.error > 0.0 && out.error < 0.1, "error {}", out.error);
    // The reported error bounds how far the surface moved: every kept vertex is still on the
    // sphere, and triangle centers are within the error of it.
    for t in out.indices.as_chunks::<3>().0 {
        let c: [f32; 3] = std::array::from_fn(|i| {
            t.iter().map(|&v| s.positions[v as usize][i]).sum::<f32>() / 3.0
        });
        let r = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
        assert!(1.0 - r <= out.error * 4.0 + 0.02, "center at radius {r}");
    }
    // The UV seam (u = 0 and u = 1 copies) is never welded away: triangles on either side keep
    // their own copies.
    let seam_low: HashSet<u32> = (0..s.uvs.len() as u32)
        .filter(|&v| s.uvs[v as usize][0] == 0.0)
        .collect();
    let seam_high: HashSet<u32> = (0..s.uvs.len() as u32)
        .filter(|&v| s.uvs[v as usize][0] == 1.0)
        .collect();
    for t in out.indices.as_chunks::<3>().0 {
        let low = t.iter().any(|v| seam_low.contains(v));
        let high = t.iter().any(|v| seam_high.contains(v));
        assert!(!(low && high), "a triangle mixes both sides of the UV seam");
    }
    // A tighter error limit stops earlier.
    let strict = simplify::simplify(&input, before / 4, out.error * 0.25);
    assert!(strict.indices.len() > out.indices.len());
    assert!(strict.error <= out.error * 0.25 + 1e-6);
}

#[test]
fn section_borders_are_locked() {
    let (positions, uvs, indices) = grid(16);
    let normals = vec![[0.0, 1.0, 0.0]; positions.len()];
    // Left half section 0, right half section 1.
    let sections: Vec<u16> = indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let cx: f32 = t.iter().map(|&v| positions[v as usize][0]).sum::<f32>() / 3.0;
            u16::from(cx > 0.5)
        })
        .collect();
    let out = simplify::simplify(
        &simplify::Input {
            positions: &positions,
            attributes: &attributes(&normals, &uvs),
            attribute_count: 5,
            indices: &indices,
            sections: &sections,
        },
        16,
        f32::MAX,
    );
    let used: HashSet<u32> = out.indices.iter().copied().collect();
    for (v, p) in positions.iter().enumerate() {
        if p[0] == 0.5 {
            assert!(
                used.contains(&(v as u32)),
                "section border vertex at {p:?} moved"
            );
        }
    }
    for (t, &s) in out.indices.as_chunks::<3>().0.iter().zip(&out.sections) {
        let cx: f32 = t.iter().map(|&v| positions[v as usize][0]).sum::<f32>() / 3.0;
        assert_eq!(u16::from(cx > 0.5), s, "triangles keep their section");
    }
}

#[test]
fn vertex_cache_order_beats_a_shuffle() {
    let (positions, _, indices) = grid(64);
    // Shuffle the triangles.
    let mut tris: Vec<[u32; 3]> = indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| [t[0], t[1], t[2]])
        .collect();
    let mut rng = Rng::new(7);
    for i in (1..tris.len()).rev() {
        let j = (rng.next_u64() % (i as u64 + 1)) as usize;
        tris.swap(i, j);
    }
    let shuffled: Vec<u32> = tris.iter().flatten().copied().collect();
    let optimized = optimize::optimize_vertex_cache(&shuffled, positions.len());
    let mut a: Vec<[u32; 3]> = shuffled
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| [t[0], t[1], t[2]])
        .collect();
    let mut b: Vec<[u32; 3]> = optimized
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| [t[0], t[1], t[2]])
        .collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "the same triangles, same winding");
    let (before, after) = (
        optimize::acmr(&shuffled, 32),
        optimize::acmr(&optimized, 32),
    );
    assert!(
        after < 0.75 && after < before * 0.5,
        "ACMR {before} → {after}"
    );

    let (fetched, old_of) = optimize::optimize_vertex_fetch(&optimized, positions.len());
    assert_eq!(old_of.len(), positions.len());
    let mut seen = 0;
    for &v in &fetched {
        assert!(v <= seen, "vertices numbered in order of first use");
        if v == seen {
            seen += 1;
        }
    }
}

#[test]
fn meshlets_cover_every_triangle_within_limits() {
    let s = sphere(48, 96);
    let m = meshlet::build(&s.indices, &s.positions, 64, 124);
    let mut covered: Vec<[u32; 3]> = Vec::new();
    for b in &m.meshlets {
        assert!(b.vertex_count <= 64 && b.triangle_count <= 124 && b.triangle_count > 0);
        let verts = &m.vertices[b.vertex_offset as usize..][..b.vertex_count as usize];
        let unique: HashSet<u32> = verts.iter().copied().collect();
        assert_eq!(
            unique.len(),
            verts.len(),
            "a meshlet lists each vertex once"
        );
        for t in m.triangles[b.triangle_offset as usize..][..b.triangle_count as usize * 3]
            .as_chunks::<3>()
            .0
        {
            covered.push([t[0], t[1], t[2]].map(|l| verts[l as usize]));
        }
    }
    let mut original: Vec<[u32; 3]> = s
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| [t[0], t[1], t[2]])
        .collect();
    original.sort();
    covered.sort();
    assert_eq!(
        original, covered,
        "every triangle exactly once, winding kept"
    );
    // Reasonably full meshlets.
    let average = (s.indices.len() / 3) as f32 / m.meshlets.len() as f32;
    assert!(average > 80.0, "average {average} triangles per meshlet");

    // Bounds hold every vertex; a culled cone means every triangle faces away.
    let mut rng = Rng::new(3);
    for b in &m.meshlets {
        let bounds = meshlet::bounds(b, &m.vertices, &m.triangles, &s.positions);
        let verts = &m.vertices[b.vertex_offset as usize..][..b.vertex_count as usize];
        for &v in verts {
            let p = s.positions[v as usize];
            let d = (0..3)
                .map(|i| (p[i] - bounds.center[i]).powi(2))
                .sum::<f32>()
                .sqrt();
            assert!(d <= bounds.radius, "vertex outside the sphere");
        }
        let axis = [0, 1, 2].map(|i| bounds.cone[i] as f32 / 127.0);
        let cutoff = bounds.cone[3] as f32 / 127.0;
        for _ in 0..32 {
            let camera = [0, 1, 2].map(|_| (rng.f32() - 0.5) * 8.0);
            let d: [f32; 3] = std::array::from_fn(|i| bounds.center[i] - camera[i]);
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            let culled =
                d[0] * axis[0] + d[1] * axis[1] + d[2] * axis[2] >= cutoff * len + bounds.radius;
            if !culled {
                continue;
            }
            for t in m.triangles[b.triangle_offset as usize..][..b.triangle_count as usize * 3]
                .as_chunks::<3>()
                .0
            {
                let [a, bb, c] =
                    [t[0], t[1], t[2]].map(|l| s.positions[verts[l as usize] as usize]);
                let e1: [f32; 3] = std::array::from_fn(|i| bb[i] - a[i]);
                let e2: [f32; 3] = std::array::from_fn(|i| c[i] - a[i]);
                let n = [
                    e1[1] * e2[2] - e1[2] * e2[1],
                    e1[2] * e2[0] - e1[0] * e2[2],
                    e1[0] * e2[1] - e1[1] * e2[0],
                ];
                let to_camera: [f32; 3] = std::array::from_fn(|i| camera[i] - a[i]);
                let facing = n[0] * to_camera[0] + n[1] * to_camera[1] + n[2] * to_camera[2];
                assert!(facing <= 1e-6, "a culled meshlet had a front face");
            }
        }
    }
}

#[test]
fn tangents_follow_u_and_split_at_mirrors() {
    let (positions, uvs, indices) = grid(4);
    let normals = vec![[0.0, 1.0, 0.0]; positions.len()];
    let t = tangent::generate(&positions, &normals, Some(&uvs), &indices);
    assert!(t.splits.is_empty());
    for tg in &t.tangents {
        assert!(
            abs_diff_eq(tg[0], 1.0, 1e-5)
                && abs_diff_eq(tg[1], 0.0, 1e-5)
                && abs_diff_eq(tg[2], 0.0, 1e-5),
            "{tg:?}"
        );
    }
    // Mirror the UVs of the right half: u = 1 - x there. Vertices on the mirror line are shared
    // by both handednesses and get split.
    let mirrored: Vec<[f32; 2]> = positions
        .iter()
        .map(|p| {
            if p[0] > 0.5 {
                [1.0 - p[0], p[2]]
            } else {
                [p[0], p[2]]
            }
        })
        .collect();
    let t = tangent::generate(&positions, &normals, Some(&mirrored), &indices);
    assert_eq!(t.splits.len(), 5, "the five vertices on x = 0.5");
    for (tri, chunk) in t.indices.as_chunks::<3>().0.iter().enumerate() {
        let signs: HashSet<i32> = chunk
            .iter()
            .map(|&v| t.tangents[v as usize][3] as i32)
            .collect();
        assert_eq!(signs.len(), 1, "triangle {tri} has one handedness");
    }
}
