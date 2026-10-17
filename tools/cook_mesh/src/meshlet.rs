//! Meshlet building: clusters of up to 64 vertices and 124 triangles, spatially compact and with
//! tight normal cones, plus their culling bounds.
//!
//! Greedy growth in the manner of meshoptimizer's builder: a meshlet grows by the adjacent
//! triangle that adds the fewest new vertices, then by distance from the meshlet's center and
//! deviation from its average normal; when nothing adjacent fits, a new meshlet starts from the
//! unused triangle nearest the last one (a uniform grid finds it). Input order matters only as a
//! tie-break, but cache-optimized input gives slightly better clusters.

use std::collections::HashMap;

/// One built meshlet, before encoding.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Built {
    /// First entry in [`Meshlets::vertices`].
    pub vertex_offset: u32,
    pub vertex_count: u32,
    /// First byte in [`Meshlets::triangles`] (3 per triangle).
    pub triangle_offset: u32,
    pub triangle_count: u32,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Meshlets {
    pub meshlets: Vec<Built>,
    /// Mesh vertex indices.
    pub vertices: Vec<u32>,
    /// Indices into each meshlet's vertices, three per triangle.
    pub triangles: Vec<u8>,
}

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

fn length(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

fn normalize(a: [f32; 3]) -> [f32; 3] {
    let l = length(a);
    if l > 0.0 { a.map(|c| c / l) } else { [0.0; 3] }
}

/// How much the normal cone counts against distance when growing (0: purely spatial).
const CONE_WEIGHT: f32 = 0.25;

/// Builds meshlets over the triangle list `indices`.
pub fn build(
    indices: &[u32],
    positions: &[[f32; 3]],
    max_vertices: u32,
    max_triangles: u32,
) -> Meshlets {
    assert!((3..=256).contains(&max_vertices) && (1..=512).contains(&max_triangles));
    let triangle_count = indices.len() / 3;
    let mut out = Meshlets::default();
    if triangle_count == 0 {
        return out;
    }
    let vertex_count = positions.len();
    let tri = |t: usize| [indices[t * 3], indices[t * 3 + 1], indices[t * 3 + 2]];
    let corners = |t: usize| tri(t).map(|v| positions[v as usize]);
    let centroids: Vec<[f32; 3]> = (0..triangle_count)
        .map(|t| {
            let [a, b, c] = corners(t);
            std::array::from_fn(|i| (a[i] + b[i] + c[i]) / 3.0)
        })
        .collect();
    let normals: Vec<[f32; 3]> = (0..triangle_count)
        .map(|t| {
            let [a, b, c] = corners(t);
            normalize(cross(sub(b, a), sub(c, a)))
        })
        .collect();
    let total_area: f32 = (0..triangle_count)
        .map(|t| {
            let [a, b, c] = corners(t);
            length(cross(sub(b, a), sub(c, a))) * 0.5
        })
        .sum();
    // The radius of a full meshlet of average triangles: distances are measured against it.
    let meshlet_radius = ((total_area / triangle_count as f32) * max_triangles as f32
        / std::f32::consts::PI)
        .sqrt()
        .max(1e-12);

    // Vertex -> live triangles.
    let mut live = vec![0u32; vertex_count];
    for &v in indices {
        live[v as usize] += 1;
    }
    let mut start = vec![0u32; vertex_count + 1];
    for v in 0..vertex_count {
        start[v + 1] = start[v] + live[v];
    }
    let mut fill = start.clone();
    let mut adjacency = vec![0u32; indices.len()];
    for t in 0..triangle_count {
        for v in tri(t) {
            adjacency[fill[v as usize] as usize] = t as u32;
            fill[v as usize] += 1;
        }
    }

    // A grid of triangle centroids for finding seeds.
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for c in &centroids {
        for i in 0..3 {
            lo[i] = lo[i].min(c[i]);
            hi[i] = hi[i].max(c[i]);
        }
    }
    let cell = (meshlet_radius * 0.5)
        .max((0..3).map(|i| hi[i] - lo[i]).fold(0.0, f32::max) / 256.0)
        .max(1e-12);
    let cell_of =
        |p: [f32; 3]| -> [i32; 3] { std::array::from_fn(|i| ((p[i] - lo[i]) / cell) as i32) };
    let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
    for (t, c) in centroids.iter().enumerate() {
        grid.entry(cell_of(*c)).or_default().push(t as u32);
    }

    let mut emitted = vec![false; triangle_count];
    let mut remaining = triangle_count;
    // Mesh vertex -> index in the current meshlet.
    let mut local = vec![u8::MAX; vertex_count];
    let mut current_vertices: Vec<u32> = Vec::new();
    let mut current_triangles: Vec<[u8; 3]> = Vec::new();
    let mut center_sum = [0.0f32; 3];
    let mut normal_sum = [0.0f32; 3];
    let mut scan = 0usize;
    let mut last_center = centroids[0];

    let flush =
        |out: &mut Meshlets, verts: &mut Vec<u32>, tris: &mut Vec<[u8; 3]>, local: &mut [u8]| {
            if tris.is_empty() {
                return;
            }
            out.meshlets.push(Built {
                vertex_offset: out.vertices.len() as u32,
                vertex_count: verts.len() as u32,
                triangle_offset: out.triangles.len() as u32,
                triangle_count: tris.len() as u32,
            });
            for &v in verts.iter() {
                local[v as usize] = u8::MAX;
            }
            out.vertices.append(verts);
            out.triangles.extend(tris.iter().flatten());
            tris.clear();
        };

    while remaining > 0 {
        // Best adjacent candidate.
        let mut best: Option<(u32, f32, usize)> = None; // (extra, cost, triangle)
        if !current_triangles.is_empty() {
            let n = current_triangles.len() as f32;
            let center = center_sum.map(|c| c / n);
            let axis = normalize(normal_sum);
            for &v in &current_vertices {
                let s = start[v as usize] as usize;
                for &t in &adjacency[s..s + live[v as usize] as usize] {
                    let t = t as usize;
                    let extra = tri(t)
                        .iter()
                        .filter(|&&x| local[x as usize] == u8::MAX)
                        .count() as u32;
                    if current_vertices.len() as u32 + extra > max_vertices {
                        continue;
                    }
                    // Priority: free triangles, then dangling ones (a vertex with no other live
                    // triangle: left behind it would start a meshlet of its own), then by new
                    // vertices.
                    let dangling = tri(t).iter().any(|&x| live[x as usize] == 1);
                    let priority = match extra {
                        0 => 0,
                        _ if dangling => 1,
                        e => e + 1,
                    };
                    let distance = length(sub(centroids[t], center)) / meshlet_radius;
                    let spread = 1.0 - dot(normals[t], axis);
                    let cost = distance * (1.0 - CONE_WEIGHT) + spread * CONE_WEIGHT;
                    let key = (priority, cost);
                    if best.is_none_or(|b| (b.0, b.1) > key || ((b.0, b.1) == key && t < b.2)) {
                        best = Some((priority, cost, t));
                    }
                }
            }
        }
        let t = match best {
            Some((_, _, t)) => t,
            None => {
                // Nothing adjacent fits. A nearby triangle may still join (closing gaps that
                // would otherwise be left as tiny meshlets); otherwise start a new meshlet.
                let growing = !current_triangles.is_empty();
                let joined = if growing {
                    let n = current_triangles.len() as f32;
                    let center = center_sum.map(|c| c / n);
                    nearest(&grid, cell_of(center), &centroids, &emitted, center, |_| 0).filter(
                        |&(t, d2)| {
                            let extra = tri(t)
                                .iter()
                                .filter(|&&x| local[x as usize] == u8::MAX)
                                .count() as u32;
                            current_vertices.len() as u32 + extra <= max_vertices
                                && d2.sqrt() <= meshlet_radius
                        },
                    )
                } else {
                    None
                };
                match joined {
                    Some((t, _)) => t,
                    None => {
                        flush(
                            &mut out,
                            &mut current_vertices,
                            &mut current_triangles,
                            &mut local,
                        );
                        center_sum = [0.0; 3];
                        normal_sum = [0.0; 3];
                        // Seed near the last meshlet, preferring triangles with few unused
                        // neighbors: islands get absorbed instead of left behind.
                        let isolation =
                            |t: usize| tri(t).iter().map(|&v| live[v as usize]).sum::<u32>();
                        match nearest(
                            &grid,
                            cell_of(last_center),
                            &centroids,
                            &emitted,
                            last_center,
                            isolation,
                        ) {
                            Some((t, _)) => t,
                            None => {
                                while emitted[scan] {
                                    scan += 1;
                                }
                                scan
                            }
                        }
                    }
                }
            }
        };

        // Add t.
        emitted[t] = true;
        remaining -= 1;
        let mut corner = [0u8; 3];
        for (k, v) in tri(t).into_iter().enumerate() {
            if local[v as usize] == u8::MAX {
                local[v as usize] = current_vertices.len() as u8;
                current_vertices.push(v);
            }
            corner[k] = local[v as usize];
            live[v as usize] -= 1;
            let (s, e) = (
                start[v as usize] as usize,
                (start[v as usize] + live[v as usize]) as usize + 1,
            );
            if let Some(i) = adjacency[s..e].iter().position(|&x| x as usize == t) {
                adjacency.swap(s + i, e - 1);
            }
        }
        current_triangles.push(corner);
        for i in 0..3 {
            center_sum[i] += centroids[t][i];
            normal_sum[i] += normals[t][i];
        }
        last_center = centroids[t];
        if current_triangles.len() as u32 >= max_triangles
            || current_vertices.len() as u32 + 1 > max_vertices
        {
            flush(
                &mut out,
                &mut current_vertices,
                &mut current_triangles,
                &mut local,
            );
            center_sum = [0.0; 3];
            normal_sum = [0.0; 3];
        }
    }
    flush(
        &mut out,
        &mut current_vertices,
        &mut current_triangles,
        &mut local,
    );
    out
}

/// Rings searched around the last meshlet before falling back to input order: beyond that the
/// remaining triangles are far away anyway, and scanning empty cells would cost more.
const SEARCH_RINGS: i32 = 8;

/// The unused triangle nearest `p` with the lowest `score`, searching the grid ring by ring
/// around `cell`; with its squared distance.
fn nearest(
    grid: &HashMap<[i32; 3], Vec<u32>>,
    cell: [i32; 3],
    centroids: &[[f32; 3]],
    emitted: &[bool],
    p: [f32; 3],
    score: impl Fn(usize) -> u32,
) -> Option<(usize, f32)> {
    let mut best: Option<((u32, f32), usize)> = None;
    let mut found_at = None;
    for ring in 0..=SEARCH_RINGS {
        // A hit in ring r can be farther than something in ring r + 1, not r + 2 (but for the
        // corners: close enough for a seed).
        if found_at.is_some_and(|r| ring > r + 1) {
            break;
        }
        for dz in -ring..=ring {
            for dy in -ring..=ring {
                for dx in -ring..=ring {
                    if dx.abs().max(dy.abs()).max(dz.abs()) != ring {
                        continue;
                    }
                    let c = [cell[0] + dx, cell[1] + dy, cell[2] + dz];
                    let Some(list) = grid.get(&c) else { continue };
                    for &t in list {
                        let t = t as usize;
                        if emitted[t] {
                            continue;
                        }
                        let d = dot(sub(centroids[t], p), sub(centroids[t], p));
                        let key = (score(t), d);
                        if best.is_none_or(|b| key < b.0 || (key == b.0 && t < b.1)) {
                            best = Some((key, t));
                        }
                    }
                }
            }
        }
        if best.is_some() && found_at.is_none() {
            found_at = Some(ring);
        }
    }
    best.map(|b| (b.1, b.0.1))
}
/// A meshlet's culling bounds.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Bounds {
    pub center: [f32; 3],
    pub radius: f32,
    /// snorm8 axis and cutoff for the cone test of `marlin_render::asset::mesh::Meshlet`.
    pub cone: [i8; 4],
}

/// A bounding sphere of `points` (Ritter's: from the extremes along the axes, grown to cover
/// every point): within a few percent of minimal.
pub fn bounding_sphere(points: &[[f32; 3]]) -> ([f32; 3], f32) {
    if points.is_empty() {
        return ([0.0; 3], 0.0);
    }
    let mut pick = (points[0], points[0]);
    let mut best = -1.0;
    for axis in 0..3 {
        let (mut lo, mut hi) = (points[0], points[0]);
        for &p in points {
            if p[axis] < lo[axis] {
                lo = p;
            }
            if p[axis] > hi[axis] {
                hi = p;
            }
        }
        let d = dot(sub(hi, lo), sub(hi, lo));
        if d > best {
            best = d;
            pick = (lo, hi);
        }
    }
    let mut center: [f32; 3] = std::array::from_fn(|i| (pick.0[i] + pick.1[i]) * 0.5);
    let mut radius = length(sub(pick.1, pick.0)) * 0.5;
    for &p in points {
        let d = length(sub(p, center));
        if d > radius {
            let grow = (d - radius) * 0.5;
            radius += grow;
            let k = grow / d;
            center = std::array::from_fn(|i| center[i] + (p[i] - center[i]) * k);
        }
    }
    // Float slack: every point strictly inside.
    (center, radius * (1.0 + 1e-6) + 1e-9)
}

/// Bounds of meshlet `m`.
pub fn bounds(m: &Built, vertices: &[u32], triangles: &[u8], positions: &[[f32; 3]]) -> Bounds {
    let verts = &vertices[m.vertex_offset as usize..][..m.vertex_count as usize];
    let points: Vec<[f32; 3]> = verts.iter().map(|&v| positions[v as usize]).collect();
    let (center, radius) = bounding_sphere(&points);
    let tris = &triangles[m.triangle_offset as usize..][..m.triangle_count as usize * 3];
    let normals: Vec<[f32; 3]> = tris
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| points[i as usize]);
            normalize(cross(sub(b, a), sub(c, a)))
        })
        .filter(|n| dot(*n, *n) > 0.0)
        .collect();
    let axis = normalize(
        normals
            .iter()
            .fold([0.0; 3], |s, n| std::array::from_fn(|i| s[i] + n[i])),
    );
    let degenerate = Bounds {
        center,
        radius,
        cone: [0, 0, 0, 127],
    };
    if dot(axis, axis) == 0.0 {
        return degenerate;
    }
    // Quantize the axis first and measure the spread against what the shader will read.
    let q: [i8; 3] = axis.map(|c| (c * 127.0).round().clamp(-127.0, 127.0) as i8);
    let qn = normalize(q.map(|c| c as f32 / 127.0));
    let min_dot = normals.iter().map(|n| dot(*n, qn)).fold(1.0f32, f32::min);
    if min_dot <= 0.1 {
        // Wider than about 84°: never cullable in practice.
        return degenerate;
    }
    // The cone of normals widened by 90° on each side: cos(α + 90°) = -sin(α).
    let cutoff = (1.0 - min_dot * min_dot).sqrt();
    // Round up, plus one step for the axis read unnormalized: conservative.
    let cutoff_q = ((cutoff * 127.0).ceil() as i32 + 1).min(127) as i8;
    Bounds {
        center,
        radius,
        cone: [q[0], q[1], q[2], cutoff_q],
    }
}
