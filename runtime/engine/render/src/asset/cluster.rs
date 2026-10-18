//! The cluster DAG of a cooked mesh (0009 §3): clusters of up to [`CLUSTER_MAX_TRIANGLES`]
//! triangles grouped into a DAG of continuous LOD, packed into streaming pages.
//!
//! # Vertices
//!
//! A cluster's vertices are its own (a vertex shared by two clusters is stored twice), so a
//! cluster decodes without anything outside its page:
//!
//! - **positions**: as the mesh's ([`quantize_position`] over the mesh bounds, or 3×f32 with
//!   `MeshFlags::POSITION_FLOAT`). Quantizing over the whole mesh, not per cluster, keeps a
//!   position shared by two clusters bit-identical in both: LOD cuts stay watertight;
//! - **normal and tangent**: [`encode_tangent_frame`] (octahedral normal, tangent angle, sign);
//! - **UVs**: unorm16 per coordinate over the cluster's range of each set ([`UvRange`]), so
//!   tiling UVs keep their precision;
//! - **color**: RGBA8; **skin**: 4×u16 joints and 4×unorm8 weights, as the mesh's.
//!
//! [`quantize_position`]: super::mesh::quantize_position
//! [`encode_tangent_frame`]: super::mesh::encode_tangent_frame

use marlin_serial::cooked::{CookedView, Pod, RelSlice};

use super::mesh::{
    MeshError, PageRange, VertexAttributes, decode_tangent_frame, dequantize_position,
};

/// Most triangles in a cluster: within mesh shader output limits on every vendor and under the
/// 256 of DXR cluster acceleration structures, so a cluster can become a CLAS later.
pub const CLUSTER_MAX_TRIANGLES: u32 = 128;
/// Most vertices a cluster references.
pub const CLUSTER_MAX_VERTICES: u32 = 128;

/// How one UV set of a cluster decodes: `offset + q * scale` per coordinate.
#[derive(Pod, Clone, Copy, PartialEq, Debug, Default)]
#[repr(C)]
pub struct UvRange {
    pub offset: [f32; 2],
    pub scale: [f32; 2],
}

impl UvRange {
    /// The range covering `uvs`.
    pub fn of(uvs: impl IntoIterator<Item = [f32; 2]>) -> Self {
        let mut lo = [f32::MAX; 2];
        let mut hi = [f32::MIN; 2];
        for uv in uvs {
            for i in 0..2 {
                lo[i] = lo[i].min(uv[i]);
                hi[i] = hi[i].max(uv[i]);
            }
        }
        if lo[0] > hi[0] {
            return Self::default();
        }
        Self {
            offset: lo,
            scale: core::array::from_fn(|i| (hi[i] - lo[i]).max(0.0) / 65535.0),
        }
    }

    pub fn quantize(&self, uv: [f32; 2]) -> [u16; 2] {
        core::array::from_fn(|i| {
            if self.scale[i] > 0.0 {
                ((uv[i] - self.offset[i]) / self.scale[i] + 0.5).clamp(0.0, 65535.0) as u16
            } else {
                0
            }
        })
    }

    pub fn dequantize(&self, q: [u16; 2]) -> [f32; 2] {
        core::array::from_fn(|i| self.offset[i] + q[i] as f32 * self.scale[i])
    }
}

/// Reads a cluster vertex's normal, tangent frame, UVs and color from a page's attribute stream
/// (for tools and CPU paths; shaders decode the same layout). `ranges` are the cluster's, one per
/// UV set.
pub fn read_cluster_attributes(
    attributes: &[u8],
    stride: usize,
    vertex: usize,
    ranges: &[UvRange],
) -> VertexAttributes {
    let v = &attributes[vertex * stride..][..stride];
    let frame = u32::from_le_bytes([v[0], v[1], v[2], v[3]]);
    let (normal, tangent, sign) = decode_tangent_frame(frame);
    let mut uvs = [[0.0; 2]; 4];
    for (set, (uv, range)) in uvs.iter_mut().zip(ranges).enumerate() {
        let at = 4 + set * 4;
        *uv = range.dequantize([
            u16::from_le_bytes([v[at], v[at + 1]]),
            u16::from_le_bytes([v[at + 2], v[at + 3]]),
        ]);
    }
    let at = 4 + ranges.len() * 4;
    let color = (stride >= at + 4).then(|| [v[at], v[at + 1], v[at + 2], v[at + 3]]);
    VertexAttributes {
        normal,
        tangent,
        bitangent_sign: sign,
        uvs,
        color,
    }
}

/// Most bytes in a page, the unit of streaming and residency.
pub const PAGE_SIZE: usize = 128 << 10;

/// [`HierarchyChild::child`]: a [`GroupPart`] index, not a node.
pub const HIERARCHY_LEAF: u32 = 1 << 31;
/// [`HierarchyChild::child`] of an unused slot.
pub const HIERARCHY_NONE: u32 = u32::MAX;
/// Children per hierarchy node.
pub const HIERARCHY_FANOUT: usize = 8;

/// At the start of every page: its counts and streams, each 16-byte aligned.
///
/// | stream     | per element                                                             |
/// |------------|-------------------------------------------------------------------------|
/// | clusters   | [`ClusterRecord`]                                                       |
/// | uv ranges  | one [`UvRange`] per UV set per cluster                                  |
/// | positions  | the mesh's position stride                                              |
/// | attributes | the mesh's attribute stride: frame, unorm16 UVs over the cluster's ranges, RGBA8 |
/// | skin       | the mesh's skin stride                                                  |
/// | triangles  | u32: three cluster-local vertex indices in bytes 0–2, the triangle's entry in its cluster's section table in byte 3 |
/// | sections   | u16 mesh section index: the clusters' section tables                    |
#[derive(Pod, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(C)]
pub struct PageHeader {
    pub cluster_count: u32,
    pub vertex_count: u32,
    pub triangle_count: u32,
    /// Entries in the section stream.
    pub section_count: u32,
    pub clusters: PageRange,
    pub uv_ranges: PageRange,
    pub positions: PageRange,
    pub attributes: PageRange,
    pub skin: PageRange,
    pub triangles: PageRange,
    pub sections: PageRange,
    pub _pad: [u32; 2],
}

/// A cluster as the culling and raster shaders read it from a page.
///
/// LOD selection (0009 §3): a cluster is drawn when its group's error, projected from
/// [`ClusterGroup::sphere`], exceeds the threshold (the group is not good enough, so it is
/// refined) while its own [`lod_error`] projected from [`lod_sphere`] does not, or while the
/// group it was made from ([`source_group`]) is not resident. Errors and spheres are monotonic
/// from children to parents, so any such cut is watertight.
///
/// Culling: the bounding sphere and the normal cone, as [`Meshlet`]'s.
///
/// [`lod_error`]: ClusterRecord::lod_error
/// [`lod_sphere`]: ClusterRecord::lod_sphere
/// [`source_group`]: ClusterRecord::source_group
/// [`Meshlet`]: super::mesh::Meshlet
#[derive(Pod, Clone, Copy, PartialEq, Debug, Default)]
#[repr(C)]
pub struct ClusterRecord {
    pub center: [f32; 3],
    pub radius: f32,
    /// The sphere and error of the simplification that made the cluster: those of
    /// [`source_group`](ClusterRecord::source_group). Level-0 clusters have error 0 and their
    /// bounding sphere.
    pub lod_sphere: [f32; 4],
    pub lod_error: f32,
    /// Normal cone axis and cutoff, snorm8 (cutoff 127: never culled).
    pub cone: [i8; 4],
    /// The group the cluster is in, whose error is the cluster's parent error.
    pub group: u32,
    /// The group the cluster was made from; [`u32::MAX`] at level 0.
    pub source_group: u32,
    /// First vertex in the page's vertex streams (and UV range set: `cluster * uv_sets`).
    pub vertex_offset: u32,
    /// First triangle in the page's triangle stream.
    pub triangle_offset: u32,
    /// First entry of the cluster's section table in the page's section stream.
    pub section_offset: u32,
    pub vertex_count: u8,
    pub triangle_count: u8,
    pub section_count: u8,
    /// DAG level, 0 finest.
    pub level: u8,
}

/// A page, resident in the mesh payload.
#[derive(Pod, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(C)]
pub struct ClusterPage {
    /// Bulk blob index.
    pub bulk: u32,
    pub cluster_count: u32,
    /// The pages that must be resident before this one is used: the other pages of its groups
    /// and the pages of the clusters its groups were simplified to (always earlier pages). A
    /// group split across pages needs all of them, so its pages stream in and out together.
    pub first_dependency: u32,
    pub dependency_count: u32,
}

/// A group of the DAG, resident.
#[derive(Pod, Clone, Copy, PartialEq, Debug, Default)]
#[repr(C)]
pub struct ClusterGroup {
    /// Encloses the LOD spheres of the group's clusters.
    pub sphere: [f32; 4],
    /// The group's simplification error (object space): the parent error of its clusters and
    /// the LOD error of the clusters made from it. +∞ for a root group, never replaced.
    pub error: f32,
    pub level: u32,
    pub first_part: u32,
    pub part_count: u32,
}

/// A group's clusters in one page: groups split across pages have several parts.
#[derive(Pod, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(C)]
pub struct GroupPart {
    pub page: u32,
    pub first_cluster: u32,
    pub cluster_count: u32,
    pub group: u32,
}

/// A child of a [`HierarchyNode`].
#[derive(Pod, Clone, Copy, PartialEq, Debug)]
#[repr(C)]
pub struct HierarchyChild {
    /// Encloses the spheres of every group below.
    pub sphere: [f32; 4],
    /// The largest group error below: when even it projects within the threshold, nothing
    /// below is drawn.
    pub max_error: f32,
    /// A node index, [`HIERARCHY_LEAF`] | a part index, or [`HIERARCHY_NONE`].
    pub child: u32,
}

impl Default for HierarchyChild {
    fn default() -> Self {
        Self {
            sphere: [0.0; 4],
            max_error: 0.0,
            child: HIERARCHY_NONE,
        }
    }
}

/// A node of the group hierarchy (a BVH over group parts with up to [`HIERARCHY_FANOUT`]
/// children, one subtree per DAG level under the root), resident. Node 0 is the root.
#[derive(Pod, Clone, Copy, PartialEq, Debug, Default)]
#[repr(C)]
pub struct HierarchyNode {
    pub children: [HierarchyChild; HIERARCHY_FANOUT],
}

/// Reads a `T` at byte `at` of `bytes`, if it is in range.
fn read<T: Pod>(bytes: &[u8], at: usize) -> Option<T> {
    let end = at.checked_add(size_of::<T>())?;
    let src = bytes.get(at..end)?;
    // SAFETY: `src` holds `size_of::<T>()` bytes and `T: Pod` is valid for any bits; the read is
    // unaligned because bulk blobs carry no alignment guarantee.
    Some(unsafe { core::ptr::read_unaligned(src.as_ptr().cast::<T>()) })
}

/// What a page is checked against: the mesh's strides and sections.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PageLayout {
    pub uv_sets: u32,
    pub position_stride: u32,
    pub attribute_stride: u32,
    pub skin_stride: u32,
    pub mesh_sections: u32,
}

/// A checked page.
#[derive(Clone, Debug)]
pub struct PageView<'a> {
    pub bytes: &'a [u8],
    pub header: PageHeader,
    layout: PageLayout,
}

impl<'a> PageView<'a> {
    /// Checks a page: its streams in range and sized by its counts, every cluster's vertices,
    /// triangles and section table in range, every triangle's indices below its cluster's
    /// vertex count and its section entry within its table, every section a mesh section. The
    /// GPU reads pages unchecked, so a page is checked once on load.
    pub fn new(bytes: &'a [u8], layout: PageLayout) -> Result<Self, MeshError> {
        let bad = MeshError::Malformed;
        if bytes.len() > PAGE_SIZE {
            return Err(bad);
        }
        let header: PageHeader = read(bytes, 0).ok_or(bad)?;
        let h = &header;
        let fits = |r: &PageRange, len: u64| {
            r.size as u64 == len
                && r.offset as u64 + r.size as u64 <= bytes.len() as u64
                && r.offset % 16 == 0
        };
        let (c, v) = (h.cluster_count as u64, h.vertex_count as u64);
        let ok = fits(&h.clusters, c * size_of::<ClusterRecord>() as u64)
            && fits(
                &h.uv_ranges,
                c * layout.uv_sets as u64 * size_of::<UvRange>() as u64,
            )
            && fits(&h.positions, v * layout.position_stride as u64)
            && fits(&h.attributes, v * layout.attribute_stride as u64)
            && fits(&h.skin, v * layout.skin_stride as u64)
            && fits(&h.triangles, h.triangle_count as u64 * 4)
            && fits(&h.sections, h.section_count as u64 * 2);
        if !ok {
            return Err(bad);
        }
        let view = Self {
            bytes,
            header,
            layout,
        };
        for i in 0..h.cluster_count as usize {
            let cl = view.cluster(i);
            let in_range =
                |first: u32, count: u8, total: u32| first as u64 + count as u64 <= total as u64;
            if cl.vertex_count as u32 > CLUSTER_MAX_VERTICES
                || cl.triangle_count as u32 > CLUSTER_MAX_TRIANGLES
                || cl.triangle_count == 0
                || cl.section_count == 0
                || !in_range(cl.vertex_offset, cl.vertex_count, h.vertex_count)
                || !in_range(cl.triangle_offset, cl.triangle_count, h.triangle_count)
                || !in_range(cl.section_offset, cl.section_count, h.section_count)
            {
                return Err(bad);
            }
            for t in 0..cl.triangle_count as usize {
                let [a, b, c, s] = view.triangle(cl.triangle_offset as usize + t);
                if [a, b, c].iter().any(|&x| x >= cl.vertex_count) || s >= cl.section_count {
                    return Err(bad);
                }
            }
            for s in 0..cl.section_count as usize {
                if view.section(cl.section_offset as usize + s) as u32 >= layout.mesh_sections {
                    return Err(bad);
                }
            }
        }
        Ok(view)
    }

    fn stream(&self, r: &PageRange) -> &'a [u8] {
        // Checked in `new`.
        &self.bytes[r.offset as usize..][..r.size as usize]
    }

    pub fn cluster(&self, i: usize) -> ClusterRecord {
        read(
            self.bytes,
            self.header.clusters.offset as usize + i * size_of::<ClusterRecord>(),
        )
        .unwrap_or_default()
    }

    /// The cluster's UV ranges, one per set.
    pub fn uv_ranges(&self, cluster: usize) -> [UvRange; 4] {
        let sets = self.layout.uv_sets as usize;
        core::array::from_fn(|s| {
            if s < sets {
                read(
                    self.bytes,
                    self.header.uv_ranges.offset as usize
                        + (cluster * sets + s) * size_of::<UvRange>(),
                )
                .unwrap_or_default()
            } else {
                UvRange::default()
            }
        })
    }

    /// Cluster-local vertex indices and section table entry.
    pub fn triangle(&self, i: usize) -> [u8; 4] {
        let t = self.stream(&self.header.triangles);
        [t[i * 4], t[i * 4 + 1], t[i * 4 + 2], t[i * 4 + 3]]
    }

    pub fn section(&self, i: usize) -> u16 {
        let s = self.stream(&self.header.sections);
        u16::from_le_bytes([s[i * 2], s[i * 2 + 1]])
    }

    pub fn positions(&self) -> &'a [u8] {
        self.stream(&self.header.positions)
    }

    pub fn attributes(&self) -> &'a [u8] {
        self.stream(&self.header.attributes)
    }

    pub fn skin(&self) -> &'a [u8] {
        self.stream(&self.header.skin)
    }

    /// The decoded position of page vertex `v`.
    pub fn position(&self, v: usize, offset: [f32; 3], scale: [f32; 3]) -> [f32; 3] {
        let p = self.positions();
        let stride = self.layout.position_stride as usize;
        let at = v * stride;
        if stride == 12 {
            core::array::from_fn(|i| {
                f32::from_le_bytes([
                    p[at + i * 4],
                    p[at + i * 4 + 1],
                    p[at + i * 4 + 2],
                    p[at + i * 4 + 3],
                ])
            })
        } else {
            let q =
                core::array::from_fn(|i| u16::from_le_bytes([p[at + i * 2], p[at + i * 2 + 1]]));
            dequantize_position(q, offset, scale)
        }
    }
}

/// A mesh section's triangles in the RT LOD's index stream.
#[derive(Pod, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(C)]
pub struct RtSection {
    pub first_index: u32,
    pub index_count: u32,
}

/// The RT LOD (0009 §3.1 item 7): a cut of the DAG with about a quarter of the source
/// triangles, unquantized, for building the BLAS. Its bulk blob holds, each 16-byte aligned:
/// positions as 3×f32, attributes and skin in the mesh's LOD layout (f16 UVs), and u32 indices
/// sorted by section.
#[derive(Pod, Clone, Copy, Debug)]
#[repr(C)]
pub struct RtLod {
    pub bulk: u32,
    pub vertex_count: u32,
    pub triangle_count: u32,
    /// The cut's object-space error threshold.
    pub error: f32,
    pub positions: PageRange,
    pub attributes: PageRange,
    pub skin: PageRange,
    pub indices: PageRange,
    /// One per mesh section.
    pub sections: RelSlice<RtSection>,
}

/// A mesh's cluster DAG, in the mesh payload: what stays resident. Pages are bulk blobs.
#[derive(Pod, Clone, Copy, Debug)]
#[repr(C)]
pub struct ClusterDag {
    /// Pages always resident: the first ones (they hold the root groups).
    pub root_page_count: u32,
    pub level_count: u32,
    pub cluster_count: u32,
    pub source_triangles: u32,
    pub pages: RelSlice<ClusterPage>,
    /// [`ClusterPage::first_dependency`] indexes this.
    pub dependencies: RelSlice<u32>,
    pub groups: RelSlice<ClusterGroup>,
    /// In group order: [`ClusterGroup::first_part`] indexes this.
    pub parts: RelSlice<GroupPart>,
    pub nodes: RelSlice<HierarchyNode>,
    pub rt: RtLod,
}

/// A checked [`ClusterDag`].
#[derive(Clone, Copy, Debug)]
pub struct ClusterDagView<'a> {
    pub header: &'a ClusterDag,
    pub pages: &'a [ClusterPage],
    pub dependencies: &'a [u32],
    pub groups: &'a [ClusterGroup],
    pub parts: &'a [GroupPart],
    pub nodes: &'a [HierarchyNode],
    pub rt_sections: &'a [RtSection],
    pub layout: PageLayout,
}

impl<'a> ClusterDagView<'a> {
    /// Checks the resident part against itself and the bulk sizes: pages and the RT LOD in
    /// range, dependencies on other existing pages, every group's parts its own and within their pages,
    /// the hierarchy a tree whose leaves are parts. Pages are checked by [`page`] once loaded.
    ///
    /// [`page`]: ClusterDagView::page
    pub fn new(
        view: &CookedView<'a>,
        dag: &'a ClusterDag,
        layout: PageLayout,
        bulk_sizes: &[u64],
    ) -> Result<Self, MeshError> {
        let bad = MeshError::Malformed;
        let pages = view.slice(&dag.pages)?;
        let dependencies = view.slice(&dag.dependencies)?;
        let groups = view.slice(&dag.groups)?;
        let parts = view.slice(&dag.parts)?;
        let nodes = view.slice(&dag.nodes)?;
        let rt_sections = view.slice(&dag.rt.sections)?;
        if pages.is_empty()
            || dag.root_page_count == 0
            || dag.root_page_count as usize > pages.len()
            || nodes.is_empty()
        {
            return Err(bad);
        }
        for (i, p) in pages.iter().enumerate() {
            let size = *bulk_sizes.get(p.bulk as usize).ok_or(bad)?;
            let deps = dependencies
                .get(p.first_dependency as usize..)
                .and_then(|d| d.get(..p.dependency_count as usize))
                .ok_or(bad)?;
            if size > PAGE_SIZE as u64
                || deps
                    .iter()
                    .any(|&d| d as usize >= pages.len() || d as usize == i)
            {
                return Err(bad);
            }
        }
        for (g, group) in groups.iter().enumerate() {
            let own = parts
                .get(group.first_part as usize..)
                .and_then(|p| p.get(..group.part_count as usize))
                .ok_or(bad)?;
            if own.is_empty() || own.iter().any(|p| p.group as usize != g) {
                return Err(bad);
            }
        }
        for p in parts {
            let page = pages.get(p.page as usize).ok_or(bad)?;
            if p.group as usize >= groups.len()
                || p.first_cluster as u64 + p.cluster_count as u64 > page.cluster_count as u64
            {
                return Err(bad);
            }
        }
        // A tree: the root unreferenced, every other node referenced at most once, so a
        // traversal from the root visits each node at most once.
        let mut referenced = alloc::vec![false; nodes.len()];
        for node in nodes {
            for c in &node.children {
                if c.child == HIERARCHY_NONE {
                    continue;
                }
                if c.child & HIERARCHY_LEAF != 0 {
                    if (c.child & !HIERARCHY_LEAF) as usize >= parts.len() {
                        return Err(bad);
                    }
                } else {
                    let n = c.child as usize;
                    if n == 0 || n >= nodes.len() || referenced[n] {
                        return Err(bad);
                    }
                    referenced[n] = true;
                }
            }
        }
        // The RT LOD.
        let rt = &dag.rt;
        let size = *bulk_sizes.get(rt.bulk as usize).ok_or(bad)?;
        let fits = |r: &PageRange, len: u64| {
            r.size as u64 == len && r.offset as u64 + r.size as u64 <= size && r.offset % 16 == 0
        };
        let v = rt.vertex_count as u64;
        if !(fits(&rt.positions, v * 12)
            && fits(&rt.attributes, v * layout.attribute_stride as u64)
            && fits(&rt.skin, v * layout.skin_stride as u64)
            && fits(&rt.indices, rt.triangle_count as u64 * 12))
            || rt_sections.len() != layout.mesh_sections as usize
        {
            return Err(bad);
        }
        let mut next = 0u64;
        for s in rt_sections {
            if s.first_index as u64 != next || s.index_count % 3 != 0 {
                return Err(bad);
            }
            next += s.index_count as u64;
        }
        if next != rt.triangle_count as u64 * 3 {
            return Err(bad);
        }
        Ok(Self {
            header: dag,
            pages,
            dependencies,
            groups,
            parts,
            nodes,
            rt_sections,
            layout,
        })
    }

    /// The dependencies of page `page`.
    pub fn page_dependencies(&self, page: usize) -> &'a [u32] {
        let p = &self.pages[page];
        // Checked in `new`.
        &self.dependencies[p.first_dependency as usize..][..p.dependency_count as usize]
    }

    /// Checks loaded page `page` ([`PageView::new`]) and that its clusters agree with the
    /// resident part: their count, and groups that exist.
    pub fn page(&self, page: usize, bytes: &'a [u8]) -> Result<PageView<'a>, MeshError> {
        let view = PageView::new(bytes, self.layout)?;
        let groups = self.groups.len() as u32;
        let ok = view.header.cluster_count == self.pages[page].cluster_count
            && (0..view.header.cluster_count as usize).all(|i| {
                let c = view.cluster(i);
                c.group < groups && (c.source_group < groups || c.source_group == u32::MAX)
            });
        if ok {
            Ok(view)
        } else {
            Err(MeshError::Malformed)
        }
    }

    /// Checks the RT LOD's blob: every index below its vertex count.
    pub fn check_rt(&self, blob: &[u8]) -> Result<(), MeshError> {
        let r = &self.header.rt.indices;
        let indices = blob
            .get(r.offset as usize..r.offset as usize + r.size as usize)
            .ok_or(MeshError::Malformed)?;
        let count = self.header.rt.vertex_count;
        if indices
            .as_chunks::<4>()
            .0
            .iter()
            .any(|i| u32::from_le_bytes(*i) >= count)
        {
            return Err(MeshError::Malformed);
        }
        Ok(())
    }
}
