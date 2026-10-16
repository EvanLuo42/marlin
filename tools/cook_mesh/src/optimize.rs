//! Index and vertex order optimizations.

use std::collections::HashMap;

/// Merges vertices whose every attribute is bitwise equal. `key(v)` returns the vertex's
/// attribute bits. Returns the remap (old vertex → new) and the number of unique vertices; new
/// vertices are numbered in order of first appearance.
pub fn weld(vertex_count: usize, key: impl Fn(usize) -> Vec<u32>) -> (Vec<u32>, usize) {
    let mut by_key: HashMap<Vec<u32>, u32> = HashMap::with_capacity(vertex_count);
    let mut remap = Vec::with_capacity(vertex_count);
    for v in 0..vertex_count {
        let next = by_key.len() as u32;
        remap.push(*by_key.entry(key(v)).or_insert(next));
    }
    let unique = by_key.len();
    (remap, unique)
}

/// Simulated post-transform cache size the scores are tuned for.
const CACHE_SIZE: usize = 32;
const CACHE_DECAY_POWER: f32 = 1.5;
const LAST_TRIANGLE_SCORE: f32 = 0.75;
const VALENCE_BOOST_SCALE: f32 = 2.0;
const VALENCE_BOOST_POWER: f32 = 0.5;

fn vertex_score(cache_position: Option<usize>, live_triangles: u32) -> f32 {
    if live_triangles == 0 {
        return -1.0;
    }
    let cache = match cache_position {
        None => 0.0,
        // The triangle just drawn: its vertices are in the cache however the next one goes.
        Some(p) if p < 3 => LAST_TRIANGLE_SCORE,
        Some(p) => {
            let scale = 1.0 / (CACHE_SIZE - 3) as f32;
            (1.0 - (p - 3) as f32 * scale)
                .max(0.0)
                .powf(CACHE_DECAY_POWER)
        }
    };
    // Prefer finishing vertices with few triangles left, so they leave the cache for good.
    cache + VALENCE_BOOST_SCALE * (live_triangles as f32).powf(-VALENCE_BOOST_POWER)
}

/// Reorders triangles for the post-transform vertex cache (Tom Forsyth, "Linear-Speed Vertex
/// Cache Optimisation", 2006). Also good for locality in general: meshlets are built from this
/// order. Returns the reordered triangle list.
pub fn optimize_vertex_cache(indices: &[u32], vertex_count: usize) -> Vec<u32> {
    let triangle_count = indices.len() / 3;
    if triangle_count == 0 {
        return Vec::new();
    }
    // Vertex -> triangles.
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
    for (t, tri) in indices.as_chunks::<3>().0.iter().enumerate() {
        for &v in tri {
            adjacency[fill[v as usize] as usize] = t as u32;
            fill[v as usize] += 1;
        }
    }

    let mut cache_position: Vec<Option<usize>> = vec![None; vertex_count];
    let mut score: Vec<f32> = (0..vertex_count)
        .map(|v| vertex_score(None, live[v]))
        .collect();
    let mut emitted = vec![false; triangle_count];
    let triangle_score = |t: usize, score: &[f32]| -> f32 {
        indices[t * 3..t * 3 + 3]
            .iter()
            .map(|&v| score[v as usize])
            .sum()
    };
    let mut tri_score: Vec<f32> = (0..triangle_count)
        .map(|t| triangle_score(t, &score))
        .collect();

    let mut cache: Vec<u32> = Vec::with_capacity(CACHE_SIZE + 3);
    let mut out = Vec::with_capacity(indices.len());
    // Fallback scan for when the cache holds no candidate: the next unemitted triangle in order.
    let mut scan = 0usize;
    let mut best: Option<usize> =
        (0..triangle_count).max_by(|&a, &b| tri_score[a].total_cmp(&tri_score[b]).then(b.cmp(&a)));
    while let Some(t) = best {
        emitted[t] = true;
        let tri = [indices[t * 3], indices[t * 3 + 1], indices[t * 3 + 2]];
        out.extend_from_slice(&tri);
        for &v in &tri {
            live[v as usize] -= 1;
            // Remove t from the vertex's live list (swap to the end of its range).
            let (s, e) = (
                start[v as usize] as usize,
                (start[v as usize] + live[v as usize]) as usize + 1,
            );
            if let Some(i) = adjacency[s..e].iter().position(|&x| x as usize == t) {
                adjacency.swap(s + i, e - 1);
            }
        }
        // New cache: this triangle's vertices first, then the old order.
        let mut next: Vec<u32> = tri.to_vec();
        for &v in &cache {
            if !tri.contains(&v) {
                next.push(v);
            }
        }
        for (i, &v) in next.iter().enumerate() {
            cache_position[v as usize] = (i < CACHE_SIZE).then_some(i);
        }
        let evicted: Vec<u32> = next.iter().skip(CACHE_SIZE).copied().collect();
        next.truncate(CACHE_SIZE);
        cache = next;

        // Rescore cached and evicted vertices, and their triangles; pick the best of those.
        best = None;
        let mut best_score = f32::MIN;
        for &v in cache.iter().chain(&evicted) {
            score[v as usize] = vertex_score(cache_position[v as usize], live[v as usize]);
        }
        for &v in cache.iter().chain(&evicted) {
            let s = start[v as usize] as usize;
            for &adj in &adjacency[s..s + live[v as usize] as usize] {
                let adj = adj as usize;
                let ts = triangle_score(adj, &score);
                tri_score[adj] = ts;
                if ts > best_score {
                    best_score = ts;
                    best = Some(adj);
                }
            }
        }
        if best.is_none() {
            while scan < triangle_count && emitted[scan] {
                scan += 1;
            }
            best = (scan < triangle_count).then_some(scan);
        }
    }
    out
}

/// Renumbers vertices in order of first use in `indices` (vertex fetch locality), dropping
/// unused ones. Returns the new indices and, for each new vertex, its old index.
pub fn optimize_vertex_fetch(indices: &[u32], vertex_count: usize) -> (Vec<u32>, Vec<u32>) {
    let mut new_of = vec![u32::MAX; vertex_count];
    let mut old_of = Vec::new();
    let indices = indices
        .iter()
        .map(|&v| {
            if new_of[v as usize] == u32::MAX {
                new_of[v as usize] = old_of.len() as u32;
                old_of.push(v);
            }
            new_of[v as usize]
        })
        .collect();
    (indices, old_of)
}

/// Average cache miss ratio (vertices transformed per triangle) of `indices` through a FIFO
/// cache of `cache_size`: a measure for tests and statistics.
pub fn acmr(indices: &[u32], cache_size: usize) -> f32 {
    let mut cache: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
    let mut misses = 0;
    for &v in indices {
        if !cache.contains(&v) {
            misses += 1;
            cache.push_back(v);
            if cache.len() > cache_size {
                cache.pop_front();
            }
        }
    }
    misses as f32 / (indices.len() / 3).max(1) as f32
}
