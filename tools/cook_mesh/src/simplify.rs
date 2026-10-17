//! Mesh simplification by edge collapse under quadric error metrics (Garland and Heckbert,
//! "Surface Simplification Using Quadric Error Metrics", 1997), with attribute costs and
//! locked borders, for LOD chains.
//!
//! Collapses are half-edge collapses: a vertex moves onto a neighbor, so no new attribute values
//! are invented and UVs, normals and colors stay exactly as authored. Vertices are grouped by
//! position ("wedges" of one corner); a position is classified as:
//!
//! - **manifold**: one wedge, closed fan, may move to any neighbor;
//! - **seam**: two wedges joined along exactly two attribute-seam edges, may move only along the
//!   seam, both wedges together, so UV and normal seams stay intact;
//! - **locked**: on an open border, a material (section) border, a non-manifold edge, or with a
//!   more complex wedge structure. Locked positions never move. Locking the open border is what
//!   keeps LODs of neighboring pieces (terrain cells, modular kits) crack-free.
//!
//! The cost of moving `u` to `v` is the summed quadric of both evaluated at `v` (squared distance
//! from the planes of the original triangles around them, area-weighted) plus the squared change
//! of each moved wedge's attributes weighted by its area. Collapses run in passes: each pass
//! takes the cheapest candidates whose neighborhoods do not overlap, rejects those that would
//! flip a triangle, applies them, and rebuilds the adjacency. Quadrics accumulate across passes,
//! so errors are always measured against the original surface.

use std::collections::HashMap;

/// A symmetric 4×4 quadric, upper triangle: a² ab ac ad b² bc bd c² cd d².
#[derive(Clone, Copy, Default, Debug)]
struct Quadric {
    m: [f64; 10],
    /// Total area weight.
    w: f64,
}

impl Quadric {
    /// The quadric of plane `n · p + d = 0` (unit `n`), weighted.
    fn plane(n: [f64; 3], d: f64, weight: f64) -> Self {
        let [a, b, c] = n;
        Self {
            m: [
                a * a,
                a * b,
                a * c,
                a * d,
                b * b,
                b * c,
                b * d,
                c * c,
                c * d,
                d * d,
            ]
            .map(|x| x * weight),
            w: weight,
        }
    }

    fn add(&mut self, o: &Self) {
        for i in 0..10 {
            self.m[i] += o.m[i];
        }
        self.w += o.w;
    }

    /// Weighted squared distance of `p` from the planes.
    fn error(&self, p: [f64; 3]) -> f64 {
        let [x, y, z] = p;
        let m = &self.m;
        let e = m[0] * x * x
            + 2.0 * m[1] * x * y
            + 2.0 * m[2] * x * z
            + 2.0 * m[3] * x
            + m[4] * y * y
            + 2.0 * m[5] * y * z
            + 2.0 * m[6] * y
            + m[7] * z * z
            + 2.0 * m[8] * z
            + m[9];
        e.max(0.0)
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn length(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Manifold,
    Seam,
    Locked,
}

/// What to simplify.
pub struct Input<'a> {
    pub positions: &'a [[f32; 3]],
    /// `attribute_count` values per vertex, already scaled by how much each matters (a unit
    /// change costs as much as moving a unit distance off the surface).
    pub attributes: &'a [f32],
    pub attribute_count: usize,
    /// Triangle list.
    pub indices: &'a [u32],
    /// One per triangle: triangles of different sections never merge, and the edges between
    /// them stay put.
    pub sections: &'a [u16],
}

pub struct Output {
    pub indices: Vec<u32>,
    pub sections: Vec<u16>,
    /// The largest geometric error of any collapse: a distance in the positions' units.
    pub error: f32,
}

/// Simplifies toward `target` triangles without exceeding `max_error` (a distance). Stops
/// early when nothing more can collapse within the limit.
pub fn simplify(input: &Input<'_>, target: usize, max_error: f32) -> Output {
    let vertex_count = input.positions.len();
    let p64: Vec<[f64; 3]> = input.positions.iter().map(|p| p.map(f64::from)).collect();

    // Wedges: vertices sharing a position.
    let mut position_of = vec![0u32; vertex_count];
    let mut positions: Vec<u32> = Vec::new(); // a representative vertex per position
    {
        let mut by_bits: HashMap<[u32; 3], u32> = HashMap::new();
        for (v, p) in input.positions.iter().enumerate() {
            let id = *by_bits.entry(p.map(f32::to_bits)).or_insert_with(|| {
                positions.push(v as u32);
                positions.len() as u32 - 1
            });
            position_of[v] = id;
        }
    }
    let position_count = positions.len();
    let mut wedges: Vec<Vec<u32>> = vec![Vec::new(); position_count];
    for v in 0..vertex_count {
        wedges[position_of[v] as usize].push(v as u32);
    }

    // Triangles with a repeated position have no area and would make self-edges.
    let mut tris: Vec<[u32; 3]> = Vec::with_capacity(input.indices.len() / 3);
    let mut sections = Vec::with_capacity(input.indices.len() / 3);
    for (t, &s) in input.indices.as_chunks::<3>().0.iter().zip(input.sections) {
        let p = [t[0], t[1], t[2]].map(|v| position_of[v as usize]);
        if p[0] != p[1] && p[1] != p[2] && p[0] != p[2] {
            tris.push([t[0], t[1], t[2]]);
            sections.push(s);
        }
    }

    // Quadrics of the original surface, per position.
    let mut quadrics = vec![Quadric::default(); position_count];
    for t in &tris {
        let [a, b, c] = t.map(|v| p64[v as usize]);
        let n = cross(sub(b, a), sub(c, a));
        let len = length(n);
        if len <= 0.0 {
            continue;
        }
        let n = n.map(|x| x / len);
        let q = Quadric::plane(n, -dot(n, a), len * 0.5);
        for &v in t {
            quadrics[position_of[v as usize] as usize].add(&q);
        }
    }

    let attr = |v: u32| {
        let n = input.attribute_count;
        &input.attributes[v as usize * n..][..n]
    };

    // Vertex remap: collapsed wedges point at their target.
    let mut remap: Vec<u32> = (0..vertex_count as u32).collect();
    let mut max_cost_error = 0.0f64;
    let max_error = f64::from(max_error);

    loop {
        let live = tris.len();
        if live <= target {
            break;
        }

        // Adjacency at the position level.
        let pos_tri = |t: &[u32; 3]| t.map(|v| position_of[v as usize]);
        // Directed position edges -> (count, triangle of the first, its vertex edge).
        let mut edges: HashMap<(u32, u32), (u32, usize)> = HashMap::with_capacity(live * 3);
        for (i, t) in tris.iter().enumerate() {
            let p = pos_tri(t);
            for k in 0..3 {
                let e = edges.entry((p[k], p[(k + 1) % 3])).or_insert((0, i));
                e.0 += 1;
            }
        }
        let mut kind = vec![Kind::Manifold; position_count];
        let mut seam_edges: Vec<u8> = vec![0; position_count];
        let mut used = vec![false; position_count];
        let mut in_use = vec![false; vertex_count];
        for (i, t) in tris.iter().enumerate() {
            let p = pos_tri(t);
            for k in 0..3 {
                let (a, b) = (p[k], p[(k + 1) % 3]);
                used[a as usize] = true;
                in_use[t[k] as usize] = true;
                let count = edges[&(a, b)].0;
                match edges.get(&(b, a)) {
                    Some(&(opposite_count, j))
                        if count == 1 && opposite_count == 1 && sections[i] == sections[j] =>
                    {
                        // An attribute seam: the triangle across uses other wedges. Each
                        // undirected edge is seen from both sides; count it from one.
                        let o = tris[j];
                        if !(o.contains(&t[k]) && o.contains(&t[(k + 1) % 3])) && a < b {
                            seam_edges[a as usize] = seam_edges[a as usize].saturating_add(1);
                            seam_edges[b as usize] = seam_edges[b as usize].saturating_add(1);
                        }
                    }
                    // Open border, non-manifold edge or section border.
                    _ => {
                        kind[a as usize] = Kind::Locked;
                        kind[b as usize] = Kind::Locked;
                    }
                }
            }
        }
        let live_wedges = |p: usize| wedges[p].iter().filter(|&&v| in_use[v as usize]).count();
        for p in 0..position_count {
            if kind[p] == Kind::Locked {
                continue;
            }
            kind[p] = match (live_wedges(p), seam_edges[p]) {
                (1, 0) => Kind::Manifold,
                (2, 2) => Kind::Seam,
                _ => Kind::Locked,
            };
        }

        // Position -> triangles.
        let mut fan_start = vec![0u32; position_count + 1];
        for t in &tris {
            for v in pos_tri(t) {
                fan_start[v as usize + 1] += 1;
            }
        }
        for i in 0..position_count {
            fan_start[i + 1] += fan_start[i];
        }
        let mut fill = fan_start.clone();
        let mut fan = vec![0u32; live * 3];
        for (i, t) in tris.iter().enumerate() {
            for v in pos_tri(t) {
                fan[fill[v as usize] as usize] = i as u32;
                fill[v as usize] += 1;
            }
        }
        let fan_of =
            |p: u32| &fan[fan_start[p as usize] as usize..fan_start[p as usize + 1] as usize];

        // Which wedge of `v` each wedge of `u` moves to: the one sharing a triangle with it.
        let wedge_map = |u: u32, v: u32| -> Option<Vec<(u32, u32)>> {
            let mut map: Vec<(u32, u32)> = Vec::with_capacity(2);
            for &ti in fan_of(u) {
                let t = tris[ti as usize];
                let p = pos_tri(&t);
                let (Some(iu), Some(iv)) = (
                    p.iter().position(|&x| x == u),
                    p.iter().position(|&x| x == v),
                ) else {
                    continue;
                };
                let (wu, wv) = (t[iu], t[iv]);
                match map.iter().find(|m| m.0 == wu) {
                    Some(m) if m.1 != wv => return None,
                    Some(_) => {}
                    None => map.push((wu, wv)),
                }
            }
            (map.len() == live_wedges(u as usize)).then_some(map)
        };

        // Candidates: the cheapest valid target of every movable position.
        let mut candidates: Vec<(f64, u32, u32)> = Vec::new();
        for u in 0..position_count as u32 {
            if !used[u as usize] || kind[u as usize] == Kind::Locked {
                continue;
            }
            let mut best: Option<(f64, u32)> = None;
            for &ti in fan_of(u) {
                let p = pos_tri(&tris[ti as usize]);
                for &v in &p {
                    if v == u || best.is_some_and(|b| b.1 == v) {
                        continue;
                    }
                    if kind[u as usize] == Kind::Seam {
                        // Along the seam only: the edge must be a seam edge.
                        let along = {
                            let t = tris[ti as usize];
                            let iu = p.iter().position(|&x| x == u).unwrap_or(0);
                            let iv = p.iter().position(|&x| x == v).unwrap_or(0);
                            let (a, b) = if (iu + 1) % 3 == iv { (u, v) } else { (v, u) };
                            edges.get(&(b, a)).is_some_and(|&(_, j)| {
                                let o = tris[j];
                                !(o.contains(&t[iu]) && o.contains(&t[iv]))
                            })
                        };
                        if !along {
                            continue;
                        }
                    }
                    let Some(map) = wedge_map(u, v) else {
                        continue;
                    };
                    let mut q = quadrics[u as usize];
                    q.add(&quadrics[v as usize]);
                    let pv = p64[positions[v as usize] as usize];
                    let geometric = q.error(pv);
                    let area_u = quadrics[u as usize].w.max(1e-30);
                    let attribute: f64 = map
                        .iter()
                        .map(|&(wu, wv)| {
                            attr(wu)
                                .iter()
                                .zip(attr(wv))
                                .map(|(a, b)| f64::from(a - b).powi(2))
                                .sum::<f64>()
                        })
                        .sum::<f64>()
                        * area_u;
                    let cost = geometric + attribute;
                    if best.is_none_or(|b| cost < b.0) {
                        best = Some((cost, v));
                    }
                }
            }
            if let Some((cost, v)) = best {
                let mut q = quadrics[u as usize];
                q.add(&quadrics[v as usize]);
                let distance =
                    (q.error(p64[positions[v as usize] as usize]) / q.w.max(1e-30)).sqrt();
                if distance <= max_error {
                    candidates.push((cost, u, v));
                }
            }
        }
        if candidates.is_empty() {
            break;
        }
        candidates.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));

        let mut touched = vec![false; position_count];
        let mut removed = 0usize;
        let budget = live - target;
        let mut collapsed_any = false;
        for &(_, u, v) in &candidates {
            if removed >= budget {
                break;
            }
            if touched[u as usize] || touched[v as usize] {
                continue;
            }
            // Reject flips and slivers among the triangles that move.
            let pv = p64[positions[v as usize] as usize];
            let mut flips = false;
            let mut gone = 0;
            for &ti in fan_of(u) {
                let p = pos_tri(&tris[ti as usize]);
                if p.contains(&v) {
                    gone += 1;
                    continue;
                }
                let corners = p.map(|x| p64[positions[x as usize] as usize]);
                let moved = p.map(|x| {
                    if x == u {
                        pv
                    } else {
                        p64[positions[x as usize] as usize]
                    }
                });
                let n0 = cross(sub(corners[1], corners[0]), sub(corners[2], corners[0]));
                let n1 = cross(sub(moved[1], moved[0]), sub(moved[2], moved[0]));
                let (l0, l1) = (length(n0), length(n1));
                if l1 <= l0 * 1e-6 || dot(n0, n1) < 0.25 * l0 * l1 {
                    flips = true;
                    break;
                }
            }
            if flips {
                continue;
            }
            let Some(map) = wedge_map(u, v) else {
                continue;
            };
            for (wu, wv) in map {
                remap[wu as usize] = wv;
            }
            let q = quadrics[u as usize];
            quadrics[v as usize].add(&q);
            let qv = &quadrics[v as usize];
            max_cost_error = max_cost_error.max(qv.error(pv) / qv.w.max(1e-30));
            touched[u as usize] = true;
            touched[v as usize] = true;
            for &ti in fan_of(u) {
                for x in pos_tri(&tris[ti as usize]) {
                    touched[x as usize] = true;
                }
            }
            removed += gone;
            collapsed_any = true;
        }
        if !collapsed_any {
            break;
        }

        // Apply: follow remap chains, drop degenerate triangles.
        let resolve = |mut v: u32| {
            while remap[v as usize] != v {
                v = remap[v as usize];
            }
            v
        };
        let mut next_tris = Vec::with_capacity(tris.len());
        let mut next_sections = Vec::with_capacity(tris.len());
        for (t, &s) in tris.iter().zip(&sections) {
            let r = t.map(resolve);
            let p = r.map(|v| position_of[v as usize]);
            if p[0] != p[1] && p[1] != p[2] && p[0] != p[2] {
                next_tris.push(r);
                next_sections.push(s);
            }
        }
        tris = next_tris;
        sections = next_sections;
    }

    Output {
        indices: tris.iter().flatten().copied().collect(),
        sections,
        error: max_cost_error.sqrt() as f32,
    }
}
