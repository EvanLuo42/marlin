//! The cluster DAG of a cooked mesh (0009 §3): clusters of up to [`CLUSTER_MAX_TRIANGLES`]
//! triangles grouped into a DAG of continuous LOD, packed into streaming pages.

/// Most triangles in a cluster: within mesh shader output limits on every vendor and under the
/// 256 of DXR cluster acceleration structures, so a cluster can become a CLAS later.
pub const CLUSTER_MAX_TRIANGLES: u32 = 128;
/// Most vertices a cluster references.
pub const CLUSTER_MAX_VERTICES: u32 = 128;
