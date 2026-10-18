//! Resident geometry (0009 §3.2): a cooked mesh's cluster DAG on the GPU. Its pages go into the
//! page pool, every page resident (R2a; streaming them is R2b), and its resident part (groups,
//! parts, hierarchy and the page table) into a buffer of its own.
//!
//! - **Page pool** ([`crate::Meshes`]): one buffer of slots of [`PAGE_SIZE`] bytes, read through
//!   one raw view. A page sits at `slot * PAGE_SIZE` as cooked, so the offsets in its header
//!   are from the slot's start.
//! - **DAG buffer**, one per mesh, one raw view (byte offsets):
//!
//! | at | what |
//! |---|---|
//! | 0 | four words: the groups', the parts' and the page table's offsets, then the node count |
//! | 16 | the hierarchy nodes ([`HierarchyNode`], node 0 the root) |
//! | groups | [`ClusterGroup`]s, in DAG order |
//! | parts | [`GroupPart`]s |
//! | page table | a u32 slot per page |
//!
//! `Marlin.Engine.Scene`'s `Geometry.mshader` reads both.

use alloc::vec::Vec;

use marlin_serial::cooked::Pod;

use crate::asset::cluster::{
    ClusterDagView, ClusterGroup, GroupPart, HIERARCHY_LEAF, HIERARCHY_NONE, HierarchyNode,
    PAGE_SIZE,
};
use crate::asset::mesh::{MeshError, MeshView};

/// Bits of a page's cluster index in a visible cluster's second word (the slot is above them).
pub const CLUSTER_INDEX_BITS: u32 = 12;

/// What the visibility passes need to size their buffers for one instance of a mesh.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct DagCounts {
    pub clusters: u32,
    pub parts: u32,
    pub nodes: u32,
    /// Levels of hierarchy nodes below and including the root: the traversal passes it takes.
    pub depth: u32,
}

/// A mesh's DAG, checked and laid out for upload.
pub(crate) struct Built<'a> {
    /// The pages in DAG order, each checked.
    pub pages: Vec<&'a [u8]>,
    pub counts: DagCounts,
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

/// Checks `mesh`'s DAG pages (`pages` are the mesh's bulk blobs, indexed by `ClusterPage::bulk`):
/// the GPU reads them unchecked. Meshes cooked without the DAG are `Malformed`: the renderer
/// draws nothing else.
pub(crate) fn build<'a>(
    mesh: &MeshView<'a>,
    pages: &[&'a [u8]],
) -> Result<(ClusterDagView<'a>, Built<'a>), MeshError> {
    let dag = mesh.cluster_dag.ok_or(MeshError::Malformed)?;
    let mut checked = Vec::with_capacity(dag.pages.len());
    for (i, p) in dag.pages.iter().enumerate() {
        let bytes = *pages.get(p.bulk as usize).ok_or(MeshError::Malformed)?;
        let view = dag.page(i, bytes)?;
        if view.header.cluster_count >= 1 << CLUSTER_INDEX_BITS {
            return Err(MeshError::Malformed);
        }
        checked.push(bytes);
    }
    let counts = DagCounts {
        clusters: dag.pages.iter().map(|p| p.cluster_count).sum(),
        parts: dag.parts.len() as u32,
        nodes: dag.nodes.len() as u32,
        // A tree (checked by `ClusterDagView::new`), so the recursion ends.
        depth: depth(dag.nodes, 0),
    };
    Ok((
        dag,
        Built {
            pages: checked,
            counts,
        },
    ))
}

/// The DAG buffer's bytes (see the module docs), the page table filled from `slots`.
pub(crate) fn dag_buffer(dag: &ClusterDagView<'_>, slots: &[u32]) -> Vec<u8> {
    let nodes = bytes_of(dag.nodes);
    let groups_at = 16 + nodes.len();
    let parts_at = groups_at + size_of_val::<[ClusterGroup]>(dag.groups);
    let table_at = parts_at + size_of_val::<[GroupPart]>(dag.parts);
    let mut out = Vec::with_capacity(table_at + slots.len() * 4);
    for w in [
        groups_at as u32,
        parts_at as u32,
        table_at as u32,
        dag.nodes.len() as u32,
    ] {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out.extend_from_slice(nodes);
    out.extend_from_slice(bytes_of(dag.groups));
    out.extend_from_slice(bytes_of(dag.parts));
    for s in slots {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Bytes of the page pool for `slots` slots.
pub(crate) const fn pool_size(slots: u32) -> u64 {
    slots as u64 * PAGE_SIZE as u64
}
