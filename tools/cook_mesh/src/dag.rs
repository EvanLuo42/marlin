//! The cluster DAG (0009 §3.1 items 2–4): continuous LOD over clusters.
//!
//! Level 0 is the mesh split into [`cluster`]s. Each level is partitioned into groups of
//! adjacent clusters (at most [`MAX_GROUP`]; recursive bisection of the cluster adjacency graph,
//! weighted by shared edges, so groups are compact and their borders short). Each group is
//! simplified to half its triangles on its own: its outer border is an open border to the
//! simplifier, so it is locked. The result is split into the clusters of the next level. This
//! repeats until a single cluster is left, which forms the root group.
//!
//! A group records its error (the largest error of its clusters plus what simplifying it
//! added) and a bounding sphere enclosing its clusters' spheres. Its clusters have it as their
//! *parent* error; the clusters made from it have it as their own (*LOD*) error. So errors and
//! spheres grow monotonically from children to parents, any per-group choice "the group, or what
//! it was simplified to" made by comparing a monotonic projection of the group's error against
//! a threshold picks a consistent cut, and because group borders never move, the cut is
//! watertight.
//!
//! A group that cannot be simplified (locked borders everywhere: many sections, a mesh of tiny
//! pieces) becomes a root: its clusters are never replaced. Its neighbors keep simplifying with
//! the shared border locked, so the cut stays watertight.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::cluster::{self, Cluster};
use crate::meshlet;
use crate::simplify;

/// Most clusters in a group. Partitions are balanced, so groups hold between half this and this
/// many clusters, except at the top of the DAG where fewer are left.
pub const MAX_GROUP: usize = 32;

/// A group is simplified when it loses at least this fraction of its triangles; less, and it
/// becomes a root (the simplifier is stuck against locked borders).
const MIN_REDUCTION: f32 = 0.15;

/// Levels are bounded: each takes about half the triangles, so a real mesh never gets close.
const MAX_LEVELS: u32 = 48;

/// What to build the DAG of: the same as [`simplify::Input`].
pub type Input<'a> = simplify::Input<'a>;

#[derive(Clone, PartialEq, Debug)]
pub struct DagCluster {
    pub cluster: Cluster,
    /// Culling bounds.
    pub bounds: meshlet::Bounds,
    /// The error of the simplification that made the cluster (0 at level 0) and the sphere it is
    /// measured over: the error and sphere of the group the cluster was made from.
    pub lod_error: f32,
    pub lod_sphere: [f32; 4],
    /// The group the cluster belongs to, whose error is the cluster's parent error.
    pub group: u32,
    pub level: u32,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Group {
    pub clusters: Vec<u32>,
    /// The group's simplification error, a distance in the mesh's units: the largest LOD error
    /// of its clusters plus what simplifying it added. +∞ for a root group (never replaced).
    pub error: f32,
    /// Encloses the LOD spheres of the group's clusters (and so all of their geometry).
    pub sphere: [f32; 4],
    pub level: u32,
    /// The clusters the group was simplified to (empty for a root group).
    pub simplified: Vec<u32>,
}

impl Group {
    pub fn is_root(&self) -> bool {
        self.error == f32::INFINITY
    }
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Dag {
    pub clusters: Vec<DagCluster>,
    pub groups: Vec<Group>,
    /// Levels including level 0.
    pub levels: u32,
}

impl Dag {
    /// The parent error of a cluster: its group's error.
    pub fn parent_error(&self, cluster: usize) -> f32 {
        self.groups[self.clusters[cluster].group as usize].error
    }

    /// The clusters of the LOD cut with object-space error threshold `threshold`: those whose
    /// own error is within it while their group's is not.
    pub fn cut(&self, threshold: f32) -> Vec<u32> {
        (0..self.clusters.len())
            .filter(|&c| {
                self.clusters[c].lod_error <= threshold && self.parent_error(c) > threshold
            })
            .map(|c| c as u32)
            .collect()
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn length(a: [f32; 3]) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

/// A sphere enclosing `spheres` (Ritter's over spheres): within a few percent of minimal.
pub fn enclose(spheres: &[[f32; 4]]) -> [f32; 4] {
    let Some(&first) = spheres.first() else {
        return [0.0; 4];
    };
    let center = |s: &[f32; 4]| [s[0], s[1], s[2]];
    // Start from the two spheres farthest apart along the first sphere's farthest.
    let far = |from: [f32; 3]| {
        spheres
            .iter()
            .max_by(|a, b| {
                (length(sub(center(a), from)) + a[3])
                    .total_cmp(&(length(sub(center(b), from)) + b[3]))
            })
            .copied()
            .unwrap_or(first)
    };
    let a = far(center(&first));
    let b = far(center(&a));
    let d = sub(center(&b), center(&a));
    let dist = length(d);
    let mut s = if dist + b[3] <= a[3] {
        a
    } else if dist + a[3] <= b[3] {
        b
    } else {
        let r = (dist + a[3] + b[3]) * 0.5;
        let k = (r - a[3]) / dist;
        let c = std::array::from_fn::<f32, 3, _>(|i| a[i] + d[i] * k);
        [c[0], c[1], c[2], r]
    };
    for t in spheres {
        let d = sub(center(t), center(&s));
        let dist = length(d);
        if dist + t[3] > s[3] {
            if dist + s[3] <= t[3] {
                s = *t;
                continue;
            }
            let r = (dist + s[3] + t[3]) * 0.5;
            let k = (r - s[3]) / dist;
            s = [s[0] + d[0] * k, s[1] + d[1] * k, s[2] + d[2] * k, r];
        }
    }
    // Float slack: every sphere strictly inside.
    s[3] = s[3] * (1.0 + 1e-6) + 1e-9;
    s
}

/// Builds the DAG of `input`.
pub fn build(input: &Input<'_>) -> Dag {
    let mut dag = Dag::default();
    // Positions by value: what adjacency and borders are made of.
    let mut position_id = vec![0u32; input.positions.len()];
    {
        let mut ids: HashMap<[u32; 3], u32> = HashMap::new();
        for (v, p) in input.positions.iter().enumerate() {
            let next = ids.len() as u32;
            position_id[v] = *ids.entry(p.map(f32::to_bits)).or_insert(next);
        }
    }

    let push = |dag: &mut Dag,
                cluster: Cluster,
                level: u32,
                lod_error: f32,
                lod_sphere: Option<[f32; 4]>| {
        let bounds = cluster.bounds(input.positions);
        let lod_sphere = lod_sphere.unwrap_or([
            bounds.center[0],
            bounds.center[1],
            bounds.center[2],
            bounds.radius,
        ]);
        dag.clusters.push(DagCluster {
            cluster,
            bounds,
            lod_error,
            lod_sphere,
            group: u32::MAX,
            level,
        });
        dag.clusters.len() as u32 - 1
    };

    let mut current: Vec<u32> = cluster::split(input.indices, input.sections, input.positions)
        .into_iter()
        .map(|c| push(&mut dag, c, 0, 0.0, None))
        .collect();
    loop {
        if current.is_empty() {
            break;
        }
        let partitions = if current.len() == 1 {
            vec![current.clone()]
        } else {
            partition(&dag, &current, &position_id)
        };
        let last = current.len() == 1 || dag.levels + 1 >= MAX_LEVELS;
        let mut next = Vec::new();
        for part in partitions {
            let group = dag.groups.len() as u32;
            let spheres: Vec<[f32; 4]> = part
                .iter()
                .map(|&c| dag.clusters[c as usize].lod_sphere)
                .collect();
            let sphere = enclose(&spheres);
            let child_error = part
                .iter()
                .map(|&c| dag.clusters[c as usize].lod_error)
                .fold(0.0f32, f32::max);
            for &c in &part {
                dag.clusters[c as usize].group = group;
            }
            let mut g = Group {
                clusters: part,
                error: f32::INFINITY,
                sphere,
                level: dag.levels,
                simplified: Vec::new(),
            };
            if !last && let Some((clusters, error)) = simplify_group(input, &dag, &g.clusters) {
                g.error = child_error + error;
                for c in clusters {
                    let id = push(&mut dag, c, g.level + 1, g.error, Some(sphere));
                    g.simplified.push(id);
                    next.push(id);
                }
            }
            dag.groups.push(g);
        }
        dag.levels += 1;
        current = next;
    }
    dag
}

/// Simplifies the triangles of `clusters` to half with their outer border locked and splits the
/// result into clusters; `None` when it does not get far enough.
fn simplify_group(input: &Input<'_>, dag: &Dag, clusters: &[u32]) -> Option<(Vec<Cluster>, f32)> {
    let mut indices: Vec<u32> = Vec::new();
    let mut sections: Vec<u16> = Vec::new();
    for &c in clusters {
        let c = &dag.clusters[c as usize].cluster;
        indices.extend_from_slice(&c.indices);
        sections.extend_from_slice(&c.sections);
    }
    // Compact to the group's vertices: the simplifier's tables are per vertex.
    let mut mesh_of: Vec<u32> = Vec::new();
    let mut local_of: HashMap<u32, u32> = HashMap::new();
    let local: Vec<u32> = indices
        .iter()
        .map(|&v| {
            *local_of.entry(v).or_insert_with(|| {
                mesh_of.push(v);
                mesh_of.len() as u32 - 1
            })
        })
        .collect();
    let n = input.attribute_count;
    let positions: Vec<[f32; 3]> = mesh_of
        .iter()
        .map(|&v| input.positions[v as usize])
        .collect();
    let attributes: Vec<f32> = mesh_of
        .iter()
        .flat_map(|&v| input.attributes[v as usize * n..][..n].iter().copied())
        .collect();
    let triangles = sections.len();
    let out = simplify::simplify(
        &simplify::Input {
            positions: &positions,
            attributes: &attributes,
            attribute_count: n,
            indices: &local,
            sections: &sections,
        },
        triangles / 2,
        f32::MAX,
    );
    let left = out.sections.len();
    if left == 0 || left as f32 > triangles as f32 * (1.0 - MIN_REDUCTION) {
        return None;
    }
    let indices: Vec<u32> = out.indices.iter().map(|&l| mesh_of[l as usize]).collect();
    Some((
        cluster::split(&indices, &out.sections, input.positions),
        out.error,
    ))
}

/// Partitions the clusters `ids` into groups of at most [`MAX_GROUP`] adjacent clusters.
fn partition(dag: &Dag, ids: &[u32], position_id: &[u32]) -> Vec<Vec<u32>> {
    let n = ids.len();
    // Adjacency: clusters sharing an edge (by position), weighted by how many.
    let mut edge_owners: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for (local, &c) in ids.iter().enumerate() {
        for t in dag.clusters[c as usize].cluster.indices.as_chunks::<3>().0 {
            for k in 0..3 {
                let (a, b) = (
                    position_id[t[k] as usize],
                    position_id[t[(k + 1) % 3] as usize],
                );
                let key = (a.min(b), a.max(b));
                let owners = edge_owners.entry(key).or_default();
                if !owners.contains(&(local as u32)) {
                    owners.push(local as u32);
                }
            }
        }
    }
    let mut weights: HashMap<(u32, u32), u32> = HashMap::new();
    for owners in edge_owners.values() {
        for (i, &a) in owners.iter().enumerate() {
            for &b in &owners[i + 1..] {
                *weights.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
    }
    let mut graph: Vec<Vec<(u32, u32)>> = vec![Vec::new(); n];
    let mut pairs: Vec<((u32, u32), u32)> = weights.into_iter().collect();
    pairs.sort_unstable();
    for ((a, b), w) in pairs {
        graph[a as usize].push((b, w));
        graph[b as usize].push((a, w));
    }
    let centers: Vec<[f32; 3]> = ids
        .iter()
        .map(|&c| dag.clusters[c as usize].bounds.center)
        .collect();

    let mut out = Vec::new();
    let mut side = vec![0u8; n];
    bisect(
        (0..n as u32).collect(),
        &graph,
        &centers,
        &mut side,
        &mut out,
    );
    out.into_iter()
        .map(|part| part.into_iter().map(|l| ids[l as usize]).collect())
        .collect()
}

/// Splits `nodes` in two recursively until every part has at most [`MAX_GROUP`] nodes. `side`
/// is scratch: 1 for nodes of the set being split, 2 for those grown into its first half.
fn bisect(
    nodes: Vec<u32>,
    graph: &[Vec<(u32, u32)>],
    centers: &[[f32; 3]],
    side: &mut [u8],
    out: &mut Vec<Vec<u32>>,
) {
    if nodes.len() <= MAX_GROUP {
        out.push(nodes);
        return;
    }
    // Balanced: as many parts on each side as the final count of parts needs.
    let parts = nodes.len().div_ceil(MAX_GROUP);
    let target = nodes.len() * (parts / 2) / parts;
    for &v in &nodes {
        side[v as usize] = 1;
    }

    // Seed at an extreme of the set's longest axis.
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for &v in &nodes {
        for i in 0..3 {
            lo[i] = lo[i].min(centers[v as usize][i]);
            hi[i] = hi[i].max(centers[v as usize][i]);
        }
    }
    let axis = (0..3)
        .max_by(|&a, &b| (hi[a] - lo[a]).total_cmp(&(hi[b] - lo[b])))
        .unwrap_or(0);
    let seed = *nodes
        .iter()
        .min_by(|&&a, &&b| {
            centers[a as usize][axis]
                .total_cmp(&centers[b as usize][axis])
                .then(a.cmp(&b))
        })
        .unwrap_or(&nodes[0]);
    let origin = centers[seed as usize];
    let distance = |v: u32| length(sub(centers[v as usize], origin)).to_bits();

    // Grow the first half from the seed, by gain (edges into it minus edges out), nearest first.
    let mut gain: HashMap<u32, i64> = HashMap::new();
    let mut heap: BinaryHeap<(i64, Reverse<u32>, Reverse<u32>)> = BinaryHeap::new();
    let mut grown = 0usize;
    let add = |v: u32,
               side: &mut [u8],
               gain: &mut HashMap<u32, i64>,
               heap: &mut BinaryHeap<(i64, Reverse<u32>, Reverse<u32>)>| {
        side[v as usize] = 2;
        for &(u, w) in &graph[v as usize] {
            if side[u as usize] == 1 {
                let g = gain.entry(u).or_insert_with(|| {
                    -(graph[u as usize]
                        .iter()
                        .filter(|e| side[e.0 as usize] != 0)
                        .map(|e| i64::from(e.1))
                        .sum::<i64>())
                });
                *g += 2 * i64::from(w);
                heap.push((*g, Reverse(distance(u)), Reverse(u)));
            }
        }
    };
    add(seed, side, &mut gain, &mut heap);
    grown += 1;
    while grown < target {
        let next = loop {
            match heap.pop() {
                Some((g, _, Reverse(v))) if side[v as usize] == 1 && gain.get(&v) == Some(&g) => {
                    break Some(v);
                }
                Some(_) => continue,
                None => break None,
            }
        };
        // Disconnected: continue from the nearest node left.
        let v = next.unwrap_or_else(|| {
            *nodes
                .iter()
                .filter(|&&v| side[v as usize] == 1)
                .min_by_key(|&&v| (distance(v), v))
                .unwrap_or(&nodes[0])
        });
        add(v, side, &mut gain, &mut heap);
        grown += 1;
    }

    refine(&nodes, graph, side);

    let (a, b): (Vec<u32>, Vec<u32>) = nodes.iter().partition(|&&v| side[v as usize] == 2);
    for &v in &nodes {
        side[v as usize] = 0;
    }
    bisect(a, graph, centers, side, out);
    bisect(b, graph, centers, side, out);
}

/// Swaps pairs of nodes across the cut (`side` 2 against 1) while that shortens it.
fn refine(nodes: &[u32], graph: &[Vec<(u32, u32)>], side: &mut [u8]) {
    // Gain of moving a node to the other side: its edges there minus its edges here.
    let gain = |v: u32, side: &[u8]| -> i64 {
        let here = side[v as usize];
        graph[v as usize]
            .iter()
            .filter(|e| side[e.0 as usize] != 0)
            .map(|&(u, w)| {
                if side[u as usize] == here {
                    -i64::from(w)
                } else {
                    i64::from(w)
                }
            })
            .sum()
    };
    let limit = nodes.len().min(256);
    for _ in 0..limit {
        let best = |s: u8, side: &[u8]| {
            nodes
                .iter()
                .filter(|&&v| side[v as usize] == s)
                .map(|&v| (gain(v, side), Reverse(v)))
                .max()
        };
        let (Some((ga, Reverse(a))), Some((gb, Reverse(b)))) = (best(2, side), best(1, side))
        else {
            break;
        };
        let between: i64 = graph[a as usize]
            .iter()
            .filter(|e| e.0 == b)
            .map(|e| i64::from(e.1))
            .sum();
        if ga + gb - 2 * between <= 0 {
            break;
        }
        side[a as usize] = 1;
        side[b as usize] = 2;
    }
}
