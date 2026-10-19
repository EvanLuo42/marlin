//! Cluster pages and the group hierarchy (0009 §3.1 item 6, §3.2): the DAG's clusters packed
//! into pages of at most [`PAGE_SIZE`] bytes, the unit of streaming, plus what stays resident:
//! the page table, the groups, their parts and a BVH over them.
//!
//! - **Order**: root groups first, then by level from coarse to fine, each level in Morton order
//!   of its groups' spheres. Pages fill in that order, so the coarse levels come first (the
//!   root pages, always resident) and a page holds neighboring groups of one level.
//! - **Parts**: pages fill completely; a group that does not fit is split, and each piece is a
//!   part. A page lists as dependencies every page of the parent groups of its groups (the
//!   groups holding the coarser clusters they were simplified to), which come earlier: they
//!   stay resident while it is, so streaming loads coarse to fine and evicts fine to coarse.
//!   After them it lists its siblings, the other pages of its groups: a split group is drawn
//!   only whole, so its pages stream in together.
//! - **Hierarchy**: a BVH over the parts of each level (8 children per node, split by the
//!   longest axis of the sphere centers), the levels' subtrees under one root, coarsest first.
//!   Each child has a sphere enclosing its groups' spheres and the largest group error below.
//! - **Compression** is not the page's business: every page is its own bulk blob, which the
//!   packager stores as its own chunk(s) with the platform's bulk codec (0003 §4: GDeflate where
//!   the GPU or DirectStorage decompresses).

use marlin_render::asset::cluster::{
    ClusterGroup, ClusterPage, ClusterRecord, GroupPart, HIERARCHY_FANOUT, HIERARCHY_LEAF,
    HierarchyChild, HierarchyNode, PAGE_SIZE, PageHeader, UvRange,
};
use marlin_render::asset::mesh::PageRange;

use crate::dag::{self, Dag};
use crate::quantize::{self, Positions, Vertices};

/// What [`pack`] made.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Packed {
    pub pages: Vec<Vec<u8>>,
    /// [`ClusterPage::bulk`] is the page index: the caller offsets it by the bulk blobs before.
    pub page_records: Vec<ClusterPage>,
    pub dependencies: Vec<u32>,
    /// Pages always resident: the first ones, through the last holding a root group.
    pub root_page_count: u32,
    /// Indexed like the DAG's groups.
    pub groups: Vec<ClusterGroup>,
    pub parts: Vec<GroupPart>,
    pub nodes: Vec<HierarchyNode>,
    /// Where each DAG cluster went: page and index in it.
    pub cluster_location: Vec<(u32, u32)>,
}

fn align16(n: usize) -> usize {
    n.next_multiple_of(16)
}

/// Page byte counts of a set of clusters.
#[derive(Clone, Copy, Default)]
struct Counts {
    clusters: usize,
    vertices: usize,
    triangles: usize,
    sections: usize,
}

impl Counts {
    fn add(&mut self, c: &dag::DagCluster) {
        self.clusters += 1;
        self.vertices += c.cluster.vertices().len();
        self.triangles += c.cluster.triangle_count();
        self.sections += section_table(&c.cluster.sections).len();
    }

    fn bytes(&self, s: &Strides) -> usize {
        align16(size_of::<PageHeader>())
            + align16(self.clusters * size_of::<ClusterRecord>())
            + align16(self.clusters * s.uv_sets * size_of::<UvRange>())
            + align16(self.vertices * s.position)
            + align16(self.vertices * s.attribute)
            + align16(self.vertices * s.skin)
            + align16(self.triangles * 4)
            + align16(self.sections * 2)
    }
}

struct Strides {
    uv_sets: usize,
    position: usize,
    attribute: usize,
    skin: usize,
}

/// The distinct sections of a cluster, in order of first use: its section table.
fn section_table(sections: &[u16]) -> Vec<u16> {
    let mut table: Vec<u16> = Vec::new();
    for &s in sections {
        if !table.contains(&s) {
            table.push(s);
        }
    }
    table
}

/// Interleaves the low 10 bits of x, y, z.
fn morton(p: [u32; 3]) -> u32 {
    let spread = |mut v: u32| {
        v &= 0x3ff;
        v = (v | (v << 16)) & 0x0300_00ff;
        v = (v | (v << 8)) & 0x0300_f00f;
        v = (v | (v << 4)) & 0x030c_30c3;
        (v | (v << 2)) & 0x0924_9249
    };
    spread(p[0]) | spread(p[1]) << 1 | spread(p[2]) << 2
}

/// Packs `dag` into pages, the vertices encoded from `vertices`.
pub fn pack(dag: &Dag, vertices: &Vertices<'_>, positions: Positions) -> Packed {
    let strides = Strides {
        uv_sets: vertices.uvs.len(),
        position: positions.stride(),
        attribute: quantize::attribute_stride(vertices),
        skin: if vertices.skin.is_some() { 12 } else { 0 },
    };

    // Group order.
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for g in &dag.groups {
        for i in 0..3 {
            lo[i] = lo[i].min(g.sphere[i]);
            hi[i] = hi[i].max(g.sphere[i]);
        }
    }
    let cell = |g: &dag::Group| {
        morton(std::array::from_fn(|i| {
            let extent = (hi[i] - lo[i]).max(1e-30);
            ((g.sphere[i] - lo[i]) / extent * 1023.0) as u32
        }))
    };
    let mut order: Vec<u32> = (0..dag.groups.len() as u32).collect();
    order.sort_by_key(|&g| {
        let group = &dag.groups[g as usize];
        (
            !group.is_root(),
            std::cmp::Reverse(group.level),
            cell(group),
            g,
        )
    });

    // Assign clusters to pages.
    let mut out = Packed {
        cluster_location: vec![(u32::MAX, 0); dag.clusters.len()],
        groups: dag
            .groups
            .iter()
            .map(|g| ClusterGroup {
                sphere: g.sphere,
                error: g.error,
                level: g.level,
                first_part: 0,
                part_count: 0,
            })
            .collect(),
        ..Packed::default()
    };
    let mut pages: Vec<Vec<u32>> = vec![Vec::new()];
    let mut counts = Counts::default();
    let mut last_root_page = 0;
    for &g in &order {
        let group = &dag.groups[g as usize];
        for &c in &group.clusters {
            let cluster = &dag.clusters[c as usize];
            let mut next = counts;
            next.add(cluster);
            if next.bytes(&strides) > PAGE_SIZE && counts.clusters > 0 {
                pages.push(Vec::new());
                counts = Counts::default();
                next = counts;
                next.add(cluster);
            }
            assert!(
                next.bytes(&strides) <= PAGE_SIZE,
                "a cluster larger than a page"
            );
            counts = next;
            let page = pages.len() as u32 - 1;
            let index = pages[page as usize].len() as u32;
            pages[page as usize].push(c);
            out.cluster_location[c as usize] = (page, index);
            // Parts: consecutive clusters of a group in one page.
            match out.parts.last_mut() {
                Some(p) if p.group == g && p.page == page => p.cluster_count += 1,
                _ => out.parts.push(GroupPart {
                    page,
                    first_cluster: index,
                    cluster_count: 1,
                    group: g,
                }),
            }
            if group.is_root() {
                last_root_page = page;
            }
        }
    }
    out.root_page_count = last_root_page + 1;
    // Parts are in group order; groups find theirs.
    let mut part_order: Vec<u32> = (0..out.parts.len() as u32).collect();
    part_order.sort_by_key(|&p| (out.parts[p as usize].group, p));
    let parts: Vec<GroupPart> = part_order.iter().map(|&p| out.parts[p as usize]).collect();
    out.parts = parts;
    for (i, p) in out.parts.iter().enumerate() {
        let g = &mut out.groups[p.group as usize];
        if g.part_count == 0 {
            g.first_part = i as u32;
        }
        g.part_count += 1;
    }

    // Dependencies: every page of the parent groups of the page's groups; siblings: the other
    // pages of its groups.
    let pages_of = |out: &Packed, g: u32| -> Vec<u32> {
        let record = out.groups[g as usize];
        out.parts[record.first_part as usize..][..record.part_count as usize]
            .iter()
            .map(|p| p.page)
            .collect()
    };
    for (page, clusters) in pages.iter().enumerate() {
        let mut groups: Vec<u32> = clusters
            .iter()
            .map(|&c| dag.clusters[c as usize].group)
            .collect();
        groups.sort_unstable();
        groups.dedup();
        let mut deps: Vec<u32> = Vec::new();
        let mut siblings: Vec<u32> = Vec::new();
        for g in groups {
            siblings.extend(pages_of(&out, g));
            for &c in &dag.groups[g as usize].simplified {
                deps.extend(pages_of(&out, dag.clusters[c as usize].group));
            }
        }
        deps.sort_unstable();
        deps.dedup();
        deps.retain(|&d| d != page as u32);
        siblings.sort_unstable();
        siblings.dedup();
        siblings.retain(|&s| s != page as u32 && !deps.contains(&s));
        out.page_records.push(ClusterPage {
            bulk: page as u32,
            cluster_count: clusters.len() as u32,
            first_dependency: out.dependencies.len() as u32,
            dependency_count: deps.len() as u32,
            sibling_count: siblings.len() as u32,
        });
        out.dependencies.extend(deps);
        out.dependencies.extend(siblings);
    }

    out.pages = pages
        .iter()
        .map(|clusters| write_page(dag, clusters, vertices, positions, &strides))
        .collect();
    out.nodes = hierarchy(dag, &out.parts);
    out
}

fn write_page(
    dag: &Dag,
    clusters: &[u32],
    vertices: &Vertices<'_>,
    positions: Positions,
    strides: &Strides,
) -> Vec<u8> {
    let mut records: Vec<ClusterRecord> = Vec::with_capacity(clusters.len());
    let mut uv_ranges: Vec<UvRange> = Vec::new();
    let mut position_bytes: Vec<u8> = Vec::new();
    let mut attribute_bytes: Vec<u8> = Vec::new();
    let mut skin_bytes: Vec<u8> = Vec::new();
    let mut triangles: Vec<u8> = Vec::new();
    let mut sections: Vec<u16> = Vec::new();
    let mut vertex_count = 0usize;
    for &c in clusters {
        let dc = &dag.clusters[c as usize];
        let cluster_vertices = dc.cluster.vertices();
        let table = section_table(&dc.cluster.sections);
        let encoded = quantize::encode(&cluster_vertices, vertices, positions);
        records.push(ClusterRecord {
            center: dc.bounds.center,
            radius: dc.bounds.radius,
            lod_sphere: dc.lod_sphere,
            lod_error: dc.lod_error,
            cone: dc.bounds.cone,
            group: dc.group,
            source_group: dc.source_group,
            vertex_offset: vertex_count as u32,
            triangle_offset: (triangles.len() / 4) as u32,
            section_offset: sections.len() as u32,
            vertex_count: cluster_vertices.len() as u8,
            triangle_count: dc.cluster.triangle_count() as u8,
            section_count: table.len() as u8,
            level: dc.level.min(255) as u8,
        });
        uv_ranges.extend_from_slice(&encoded.uv_ranges);
        position_bytes.extend_from_slice(&encoded.positions);
        attribute_bytes.extend_from_slice(&encoded.attributes);
        skin_bytes.extend_from_slice(&encoded.skin);
        for (t, s) in dc
            .cluster
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .zip(&dc.cluster.sections)
        {
            let local = t.map(|v| cluster_vertices.iter().position(|&x| x == v).unwrap_or(0) as u8);
            let entry = table.iter().position(|x| x == s).unwrap_or(0) as u8;
            triangles.extend_from_slice(&[local[0], local[1], local[2], entry]);
        }
        sections.extend_from_slice(&table);
        vertex_count += cluster_vertices.len();
    }

    let mut page = vec![0u8; size_of::<PageHeader>()];
    let stream = |page: &mut Vec<u8>, bytes: &[u8]| -> PageRange {
        page.resize(align16(page.len()), 0);
        let offset = page.len() as u32;
        page.extend_from_slice(bytes);
        PageRange {
            offset,
            size: bytes.len() as u32,
        }
    };
    let clusters_range = stream(&mut page, pod_bytes(&records));
    let uv_range = stream(&mut page, pod_bytes(&uv_ranges));
    let positions_range = stream(&mut page, &position_bytes);
    let attributes_range = stream(&mut page, &attribute_bytes);
    let skin_range = stream(&mut page, &skin_bytes);
    let triangles_range = stream(&mut page, &triangles);
    let section_bytes: Vec<u8> = sections.iter().flat_map(|s| s.to_le_bytes()).collect();
    let sections_range = stream(&mut page, &section_bytes);
    page.resize(align16(page.len()), 0);
    debug_assert_eq!(strides.position * vertex_count, position_bytes.len());
    let header = PageHeader {
        cluster_count: records.len() as u32,
        vertex_count: vertex_count as u32,
        triangle_count: (triangles.len() / 4) as u32,
        section_count: sections.len() as u32,
        clusters: clusters_range,
        uv_ranges: uv_range,
        positions: positions_range,
        attributes: attributes_range,
        skin: skin_range,
        triangles: triangles_range,
        sections: sections_range,
        _pad: [0; 2],
    };
    page[..size_of::<PageHeader>()].copy_from_slice(pod_bytes(std::slice::from_ref(&header)));
    debug_assert!(page.len() <= PAGE_SIZE);
    page
}

/// The bytes of `values`.
fn pod_bytes<T: marlin_serial::cooked::Pod>(values: &[T]) -> &[u8] {
    // SAFETY: `Pod` types have no padding, so every byte is initialized.
    unsafe { std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), size_of_val(values)) }
}

/// A leaf or subtree of the hierarchy, before it is placed.
struct Item {
    sphere: [f32; 4],
    max_error: f32,
    child: u32,
}

/// The group hierarchy over `parts`: node 0 is the root.
fn hierarchy(dag: &Dag, parts: &[GroupPart]) -> Vec<HierarchyNode> {
    let mut nodes = vec![HierarchyNode::default()];
    let levels = dag.groups.iter().map(|g| g.level + 1).max().unwrap_or(0);
    // One subtree per level, coarsest first.
    let mut tops: Vec<Item> = Vec::new();
    for level in (0..levels).rev() {
        let mut items: Vec<Item> = parts
            .iter()
            .enumerate()
            .filter(|(_, p)| dag.groups[p.group as usize].level == level)
            .map(|(i, p)| {
                let g = &dag.groups[p.group as usize];
                Item {
                    sphere: g.sphere,
                    max_error: g.error,
                    child: HIERARCHY_LEAF | i as u32,
                }
            })
            .collect();
        if !items.is_empty() {
            tops.push(subtree(&mut items, &mut nodes));
        }
    }
    // The root holds the levels' subtrees; more levels than fit nest in groups of eight.
    while tops.len() > HIERARCHY_FANOUT {
        let mut nested = Vec::new();
        let mut rest = tops.into_iter().peekable();
        while rest.peek().is_some() {
            let mut chunk: Vec<Item> = rest.by_ref().take(HIERARCHY_FANOUT).collect();
            nested.push(if chunk.len() == 1 {
                chunk.remove(0)
            } else {
                place(&chunk, &mut nodes)
            });
        }
        tops = nested;
    }
    for (slot, item) in nodes[0].children.iter_mut().zip(&tops) {
        *slot = HierarchyChild {
            sphere: item.sphere,
            max_error: item.max_error,
            child: item.child,
        };
    }
    nodes
}

/// Places up to eight items in a new node; the item for it.
fn place(items: &[Item], nodes: &mut Vec<HierarchyNode>) -> Item {
    debug_assert!(items.len() <= HIERARCHY_FANOUT);
    let index = nodes.len() as u32;
    nodes.push(HierarchyNode::default());
    for (slot, item) in nodes[index as usize].children.iter_mut().zip(items.iter()) {
        *slot = HierarchyChild {
            sphere: item.sphere,
            max_error: item.max_error,
            child: item.child,
        };
    }
    let spheres: Vec<[f32; 4]> = items.iter().map(|i| i.sphere).collect();
    Item {
        sphere: dag::enclose(&spheres),
        max_error: items.iter().map(|i| i.max_error).fold(0.0, f32::max),
        child: index,
    }
}

/// A BVH over `items`: one item, or a node of up to eight children, each a subtree of an
/// eighth of the items split along the longest axis of their centers.
fn subtree(items: &mut [Item], nodes: &mut Vec<HierarchyNode>) -> Item {
    if items.len() == 1 {
        return Item {
            sphere: items[0].sphere,
            max_error: items[0].max_error,
            child: items[0].child,
        };
    }
    if items.len() <= HIERARCHY_FANOUT {
        return place(items, nodes);
    }
    // Three binary splits: eight buckets.
    let mut buckets: Vec<&mut [Item]> = vec![items];
    for _ in 0..3 {
        let mut next = Vec::new();
        for b in buckets {
            if b.len() <= 1 {
                next.push(b);
                continue;
            }
            let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
            for i in b.iter() {
                for a in 0..3 {
                    lo[a] = lo[a].min(i.sphere[a]);
                    hi[a] = hi[a].max(i.sphere[a]);
                }
            }
            let axis = (0..3)
                .max_by(|&x, &y| (hi[x] - lo[x]).total_cmp(&(hi[y] - lo[y])))
                .unwrap_or(0);
            b.sort_by(|x, y| {
                x.sphere[axis]
                    .total_cmp(&y.sphere[axis])
                    .then(x.child.cmp(&y.child))
            });
            let mid = b.len() / 2;
            let (l, r) = b.split_at_mut(mid);
            next.push(l);
            next.push(r);
        }
        buckets = next;
    }
    let children: Vec<Item> = buckets
        .into_iter()
        .filter(|b| !b.is_empty())
        .map(|b| subtree(b, nodes))
        .collect();
    place(&children, nodes)
}
