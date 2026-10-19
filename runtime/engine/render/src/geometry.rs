//! Resident geometry (0009 §3.2, §6.1): a cooked mesh's cluster DAG on the GPU. Its pages
//! stream through the page pool ([`crate::Meshes`]); its resident part (groups, parts and
//! hierarchy) goes into a buffer of its own.
//!
//! - **Page pool**: one buffer, read through one raw view: slots of [`PAGE_SIZE`] bytes, then
//!   the page table, a u32 per page of every mesh ([`NOT_RESIDENT`] or the page's slot). A
//!   page sits at `slot * PAGE_SIZE` as cooked, so the offsets in its header are from the
//!   slot's start. Each mesh owns a run of table entries.
//! - **DAG buffer**, one per mesh, one raw view (byte offsets):
//!
//! | at | what |
//! |---|---|
//! | 0 | four words: the groups' and the parts' offsets, the byte offset of the mesh's page table entries in the pool, the node count |
//! | 16 | the hierarchy nodes ([`HierarchyNode`], node 0 the root) |
//! | groups | [`ClusterGroup`]s, in DAG order |
//! | parts | [`GroupPart`]s |
//!
//! `Marlin.Engine.Scene`'s `Geometry.mshader` reads both.

use alloc::vec::Vec;

use marlin_serial::cooked::Pod;

use crate::asset::cluster::{
    ClusterDagView, ClusterGroup, GroupPart, HIERARCHY_LEAF, HIERARCHY_NONE, HierarchyNode,
    PAGE_SIZE,
};

/// Bits of a page's cluster index in a visible cluster's second word (the slot is above them).
pub const CLUSTER_INDEX_BITS: u32 = 12;

/// A page table entry of a page with no slot.
pub const NOT_RESIDENT: u32 = u32::MAX;

/// What the visibility passes need to size their buffers for one instance of a mesh.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct DagCounts {
    pub clusters: u32,
    pub parts: u32,
    pub nodes: u32,
    /// Levels of hierarchy nodes below and including the root: the traversal passes it takes.
    pub depth: u32,
}

fn bytes_of<T: Pod>(values: &[T]) -> &[u8] {
    // SAFETY: `T: Pod` has no padding and any bytes are valid to read.
    unsafe { core::slice::from_raw_parts(values.as_ptr().cast::<u8>(), size_of_val(values)) }
}

/// Node levels of the tree under `node`.
fn depth(nodes: &[HierarchyNode], node: usize) -> u32 {
    let below = nodes[node]
        .children
        .iter()
        .filter(|c| c.child != HIERARCHY_NONE && c.child & HIERARCHY_LEAF == 0)
        .map(|c| depth(nodes, c.child as usize))
        .max()
        .unwrap_or(0);
    1 + below
}

pub(crate) fn counts(dag: &ClusterDagView<'_>) -> DagCounts {
    DagCounts {
        clusters: dag.pages.iter().map(|p| p.cluster_count).sum(),
        parts: dag.parts.len() as u32,
        nodes: dag.nodes.len() as u32,
        // A tree (checked by `ClusterDagView::new`), so the recursion ends.
        depth: depth(dag.nodes, 0),
    }
}

/// Checks page `page` of `dag` (`bytes`) for the GPU, which reads it unchecked.
pub(crate) fn check_page(dag: &ClusterDagView<'_>, page: usize, bytes: &[u8]) -> bool {
    dag.page(page, bytes)
        .is_ok_and(|v| v.header.cluster_count < 1 << CLUSTER_INDEX_BITS)
}

/// The DAG buffer's bytes (see the module docs); the mesh's page table entries start at byte
/// `table` of the pool.
pub(crate) fn dag_buffer(dag: &ClusterDagView<'_>, table: u32) -> Vec<u8> {
    let nodes = bytes_of(dag.nodes);
    let groups_at = 16 + nodes.len();
    let parts_at = groups_at + size_of_val::<[ClusterGroup]>(dag.groups);
    let mut out = Vec::with_capacity(parts_at + size_of_val::<[GroupPart]>(dag.parts));
    for w in [
        groups_at as u32,
        parts_at as u32,
        table,
        dag.nodes.len() as u32,
    ] {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out.extend_from_slice(nodes);
    out.extend_from_slice(bytes_of(dag.groups));
    out.extend_from_slice(bytes_of(dag.parts));
    out
}

/// Byte offset of the page table in a pool of `slots` slots.
pub(crate) const fn table_offset(slots: u32) -> u64 {
    slots as u64 * PAGE_SIZE as u64
}
