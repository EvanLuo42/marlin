//! The RT LOD (0009 §3.1 item 7, §7): one cut of the cluster DAG, stored apart without
//! quantization for building the mesh's BLAS.
//!
//! The cut is the DAG's object-space cut ([`Dag::cut`]) at the threshold whose triangle count
//! comes closest to a fraction of the source's (a quarter by default). Any uniform cut is
//! watertight, so the BLAS has no cracks either.

use crate::dag::Dag;

/// The default share of the source triangles the RT LOD keeps.
pub const DEFAULT_FRACTION: f32 = 0.25;

/// The RT LOD over the mesh's vertices.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct RtLod {
    /// The cut's error threshold (object space).
    pub error: f32,
    /// Mesh vertices used, in order of first use.
    pub vertices: Vec<u32>,
    /// Triangle list into `vertices`, sorted by section.
    pub indices: Vec<u32>,
    /// Per mesh section: first index and index count.
    pub sections: Vec<(u32, u32)>,
}

fn triangles(dag: &Dag, cut: &[u32]) -> usize {
    cut.iter()
        .map(|&c| dag.clusters[c as usize].cluster.triangle_count())
        .sum()
}

/// The cut of `dag` with about `fraction` of `source_triangles`, over `section_count` sections.
pub fn build(dag: &Dag, source_triangles: usize, fraction: f32, section_count: usize) -> RtLod {
    // Candidate thresholds: 0 (the source) and every finite group error. Triangle counts fall
    // as the threshold grows, so a binary search finds the crossing.
    let mut thresholds: Vec<f32> = dag
        .groups
        .iter()
        .map(|g| g.error)
        .filter(|e| e.is_finite())
        .collect();
    thresholds.push(0.0);
    thresholds.sort_by(f32::total_cmp);
    thresholds.dedup();
    let target = (source_triangles as f32 * fraction.clamp(0.0, 1.0)) as usize;
    let count = |t: f32| triangles(dag, &dag.cut(t));
    // The first threshold at or under the target.
    let (mut lo, mut hi) = (0usize, thresholds.len() - 1);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if count(thresholds[mid]) <= target {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    let mut pick = lo;
    if lo > 0 {
        let (over, under) = (count(thresholds[lo - 1]), count(thresholds[lo]));
        if over.abs_diff(target) < under.abs_diff(target) {
            pick = lo - 1;
        }
    }
    let error = thresholds[pick];

    let mut by_section: Vec<Vec<u32>> = vec![Vec::new(); section_count];
    for c in dag.cut(error) {
        let cluster = &dag.clusters[c as usize].cluster;
        for (t, &s) in cluster
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .zip(&cluster.sections)
        {
            by_section[s as usize].extend_from_slice(t);
        }
    }
    let mut out = RtLod {
        error,
        ..RtLod::default()
    };
    let mut local: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for tris in &by_section {
        let first = out.indices.len() as u32;
        for &v in tris {
            let l = *local.entry(v).or_insert_with(|| {
                out.vertices.push(v);
                out.vertices.len() as u32 - 1
            });
            out.indices.push(l);
        }
        out.sections.push((first, out.indices.len() as u32 - first));
    }
    out
}
