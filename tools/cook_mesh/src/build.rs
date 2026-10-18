//! Imported geometry to a cooked mesh: weld, the cluster DAG and its pages, the RT LOD, and the
//! payload of `marlin_render::asset::mesh`.

use core::mem::offset_of;

use marlin_render::asset::Mesh;
use marlin_render::asset::cluster::{ClusterDag, RtLod, RtSection};
use marlin_render::asset::mesh::{
    CookedMesh, MeshFlags, MeshJoint, MeshSection, PageRange, PositionPrecision,
    encode_tangent_frame, f32_to_f16, position_scale,
};
use marlin_serial::cooked::{CookedBuilder, RelSlice};

use crate::import::Geometry;
use crate::meshlet;
use crate::optimize;
use crate::{dag, page, quantize, rt};

/// What [`build`] made: the payload and the bulk blobs: the cluster DAG's pages, then the RT
/// LOD.
pub struct Built {
    pub payload: Vec<u8>,
    pub pages: Vec<Vec<u8>>,
    /// Cluster DAG levels and clusters.
    pub dag_levels: u32,
    pub dag_clusters: usize,
}

/// How much attribute changes cost against geometric error, per unit of mesh radius: a normal
/// turning fully around costs like moving 5% of the radius off the surface.
const NORMAL_WEIGHT: f32 = 0.05;
const UV_WEIGHT: f32 = 0.1;
const COLOR_WEIGHT: f32 = 0.05;

/// The top four influences, weights in unorm8 summing to exactly 255 (largest remainders get
/// the rounding), joints renumbered by `joint`.
fn quantize_influences(inf: &[(u16, f32); 8]) -> ([u16; 4], [u8; 4]) {
    let top: [(u16, f32); 4] = std::array::from_fn(|i| (inf[i].0, inf[i].1.max(0.0)));
    let sum: f32 = top.iter().map(|t| t.1).sum();
    if sum <= 0.0 {
        return ([top[0].0, 0, 0, 0], [255, 0, 0, 0]);
    }
    let exact: [f32; 4] = std::array::from_fn(|i| top[i].1 / sum * 255.0);
    let mut q: [u8; 4] = exact.map(|e| e.floor() as u8);
    let mut left = 255 - q.iter().map(|&x| x as u32).sum::<u32>();
    let mut order = [0, 1, 2, 3];
    order.sort_by(|&a, &b| {
        (exact[b] - exact[b].floor())
            .total_cmp(&(exact[a] - exact[a].floor()))
            .then(a.cmp(&b))
    });
    for &i in order.iter().cycle() {
        if left == 0 {
            break;
        }
        q[i] += 1;
        left -= 1;
    }
    let joints = std::array::from_fn(|i| if q[i] > 0 { top[i].0 } else { 0 });
    (joints, q)
}

/// Geometry vertex `v` in the LOD attribute layout: tangent frame, f16 UVs, RGBA8 color.
fn write_attributes(
    out: &mut Vec<u8>,
    g: &Geometry,
    v: usize,
    uv_sets: usize,
    colors: Option<&Vec<[f32; 4]>>,
) {
    let t = g.tangents[v];
    out.extend(encode_tangent_frame(g.normals[v], [t[0], t[1], t[2]], t[3]).to_le_bytes());
    for set in &g.uvs[..uv_sets] {
        out.extend(set[v].iter().flat_map(|c| f32_to_f16(*c).to_le_bytes()));
    }
    if let Some(c) = colors {
        out.extend(c[v].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8));
    }
}

fn align16(v: &mut Vec<u8>) -> u32 {
    let at = v.len().next_multiple_of(16);
    v.resize(at, 0);
    at as u32
}

/// Cooks `geometry` with `settings`.
pub fn build(geometry: &Geometry, settings: &Mesh) -> Result<Built, String> {
    let g = geometry;
    let n = g.vertex_count();
    let skinned = g.influences.is_some() && !g.skin.is_empty();
    let colors = g.colors.as_ref().filter(|_| settings.vertex_colors);
    let uv_sets = g.uvs.len().min(4);
    let skin_q: Vec<([u16; 4], [u8; 4])> = match &g.influences {
        Some(inf) if skinned => inf.iter().map(quantize_influences).collect(),
        _ => Vec::new(),
    };

    // Weld identical vertices (importers emit per-primitive copies; flat normals unweld).
    let (weld, unique) = optimize::weld(n, |v| {
        let mut key: Vec<u32> = Vec::with_capacity(24);
        key.extend(g.positions[v].map(f32::to_bits));
        key.extend(g.normals[v].map(f32::to_bits));
        key.extend(g.tangents[v].map(f32::to_bits));
        for set in &g.uvs[..uv_sets] {
            key.extend(set[v].map(f32::to_bits));
        }
        if let Some(c) = colors {
            key.extend(c[v].map(f32::to_bits));
        }
        if skinned {
            let (j, w) = skin_q[v];
            key.extend(j.map(u32::from));
            key.push(u32::from_le_bytes(w));
        }
        key
    });
    let mut first = vec![u32::MAX; unique];
    for (v, &w) in weld.iter().enumerate() {
        if first[w as usize] == u32::MAX {
            first[w as usize] = v as u32;
        }
    }
    let source = |w: usize| first[w] as usize;
    let positions: Vec<[f32; 3]> = (0..unique).map(|w| g.positions[source(w)]).collect();
    let indices: Vec<u32> = g.indices.iter().map(|&i| weld[i as usize]).collect();

    // Bounds.
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for p in &positions {
        for i in 0..3 {
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    }
    let (center, radius) = meshlet::bounding_sphere(&positions);
    if !(radius.is_finite() && lo.iter().chain(&hi).all(|c| c.is_finite())) {
        return Err("the mesh has non-finite positions".into());
    }

    // Attributes the simplifier weighs.
    let r = radius.max(1e-6);
    let attribute_count = 3 + 2 * uv_sets.min(1) + if colors.is_some() { 4 } else { 0 };
    let mut attributes = Vec::with_capacity(unique * attribute_count);
    let uv_extent = g.uvs.first().map_or(1.0, |set| {
        let (mut a, mut b) = ([f32::MAX; 2], [f32::MIN; 2]);
        for uv in set {
            for i in 0..2 {
                a[i] = a[i].min(uv[i]);
                b[i] = b[i].max(uv[i]);
            }
        }
        (b[0] - a[0]).max(b[1] - a[1]).max(1e-6)
    });
    for w in 0..unique {
        let v = source(w);
        attributes.extend(g.normals[v].map(|c| c * NORMAL_WEIGHT * r));
        if uv_sets > 0 {
            attributes.extend(g.uvs[0][v].map(|c| c / uv_extent * UV_WEIGHT * r));
        }
        if let Some(c) = colors {
            attributes.extend(c[v].map(|c| c * COLOR_WEIGHT * r));
        }
    }

    let section_count = g.section_names.len();
    let position_float = settings.precision == PositionPrecision::Float;
    let scale = position_scale(lo, hi);
    let position_stride = if position_float { 12 } else { 8 };
    let attribute_stride = 4 + 4 * uv_sets as u32 + if colors.is_some() { 4 } else { 0 };
    let skin_stride = if skinned { 12 } else { 0 };

    // The cluster DAG: its pages, then the RT LOD's blob.
    let normals: Vec<[f32; 3]> = (0..unique).map(|w| g.normals[source(w)]).collect();
    let tangents: Vec<[f32; 4]> = (0..unique).map(|w| g.tangents[source(w)]).collect();
    let uvs: Vec<Vec<[f32; 2]>> = g.uvs[..uv_sets]
        .iter()
        .map(|set| (0..unique).map(|w| set[source(w)]).collect())
        .collect();
    let welded_colors: Option<Vec<[f32; 4]>> =
        colors.map(|c| (0..unique).map(|w| c[source(w)]).collect());
    let welded_skin: Option<Vec<([u16; 4], [u8; 4])>> =
        skinned.then(|| (0..unique).map(|w| skin_q[source(w)]).collect());
    let dag = dag::build(&dag::Input {
        positions: &positions,
        attributes: &attributes,
        attribute_count,
        indices: &indices,
        sections: &g.triangle_sections,
    });
    let mut packed = page::pack(
        &dag,
        &quantize::Vertices {
            positions: &positions,
            normals: &normals,
            tangents: &tangents,
            uvs: &uvs,
            colors: welded_colors.as_deref(),
            skin: welded_skin.as_deref(),
        },
        if position_float {
            quantize::Positions::Float
        } else {
            quantize::Positions::Quantized { min: lo, scale }
        },
    );
    let lod = rt::build(&dag, indices.len() / 3, settings.rt_fraction, section_count);
    let mut pages = core::mem::take(&mut packed.pages);

    let mut blob: Vec<u8> = Vec::new();
    let range = |blob: &mut Vec<u8>, bytes: Vec<u8>| {
        let offset = align16(blob);
        blob.extend_from_slice(&bytes);
        PageRange {
            offset,
            size: bytes.len() as u32,
        }
    };
    let rt_positions = range(
        &mut blob,
        lod.vertices
            .iter()
            .flat_map(|&w| positions[w as usize])
            .flat_map(f32::to_le_bytes)
            .collect(),
    );
    let mut rt_attributes = Vec::new();
    for &w in &lod.vertices {
        write_attributes(&mut rt_attributes, g, source(w as usize), uv_sets, colors);
    }
    let rt_attributes = range(&mut blob, rt_attributes);
    let rt_skin = range(
        &mut blob,
        lod.vertices
            .iter()
            .filter(|_| skinned)
            .flat_map(|&w| {
                let (j, wt) = skin_q[source(w as usize)];
                j.iter()
                    .flat_map(|c| c.to_le_bytes())
                    .chain(wt)
                    .collect::<Vec<u8>>()
            })
            .collect(),
    );
    let rt_indices = range(
        &mut blob,
        lod.indices.iter().flat_map(|i| i.to_le_bytes()).collect(),
    );
    align16(&mut blob);
    let rt_record = RtLod {
        bulk: pages.len() as u32,
        vertex_count: lod.vertices.len() as u32,
        triangle_count: (lod.indices.len() / 3) as u32,
        error: lod.error,
        positions: rt_positions,
        attributes: rt_attributes,
        skin: rt_skin,
        indices: rt_indices,
        sections: RelSlice::EMPTY,
    };
    pages.push(blob);

    // The payload.
    let mut b = CookedBuilder::new();
    let root = b.push(&CookedMesh {
        bounds_min: lo,
        bounds_max: hi,
        sphere: [center[0], center[1], center[2], radius],
        position_offset: if position_float { [0.0; 3] } else { lo },
        position_scale: if position_float { [1.0; 3] } else { scale },
        flags: if position_float {
            MeshFlags::POSITION_FLOAT
        } else {
            0
        } | if skinned { MeshFlags::SKIN } else { 0 }
            | if colors.is_some() {
                MeshFlags::COLOR
            } else {
                0
            },
        uv_sets: uv_sets as u32,
        position_stride,
        attribute_stride,
        skin_stride,
        sections: RelSlice::EMPTY,
        joints: RelSlice::EMPTY,
        cluster_dag: ClusterDag {
            root_page_count: packed.root_page_count,
            level_count: dag.levels,
            cluster_count: dag.clusters.len() as u32,
            source_triangles: (indices.len() / 3) as u32,
            pages: RelSlice::EMPTY,
            dependencies: RelSlice::EMPTY,
            groups: RelSlice::EMPTY,
            parts: RelSlice::EMPTY,
            nodes: RelSlice::EMPTY,
            rt: rt_record,
        },
    });
    let err = |e: marlin_serial::cooked::LayoutError| e.to_string();
    let section_records = b.push_slice(&vec![
        MeshSection {
            name: RelSlice::EMPTY
        };
        section_count
    ]);
    let joint_records = b.push_slice(
        &g.skin
            .iter()
            .filter(|_| skinned)
            .map(|j| MeshJoint {
                name: RelSlice::EMPTY,
                inverse_bind: j.inverse_bind,
            })
            .collect::<Vec<_>>(),
    );
    b.link_slice(
        root.field(offset_of!(CookedMesh, sections), |m| &m.sections),
        section_records,
    )
    .map_err(err)?;
    b.link_slice(
        root.field(offset_of!(CookedMesh, joints), |m| &m.joints),
        joint_records,
    )
    .map_err(err)?;
    for (i, name) in g.section_names.iter().enumerate() {
        let s = b.push_str(name);

        b.link_slice(
            section_records
                .at(i)
                .field(offset_of!(MeshSection, name), |s| &s.name),
            s,
        )
        .map_err(err)?;
    }
    if skinned {
        for (i, j) in g.skin.iter().enumerate() {
            let s = b.push_str(&j.name);

            b.link_slice(
                joint_records
                    .at(i)
                    .field(offset_of!(MeshJoint, name), |j| &j.name),
                s,
            )
            .map_err(err)?;
        }
    }
    let at = offset_of!(CookedMesh, cluster_dag);
    let s = b.push_slice(&packed.page_records);
    b.link_slice(
        root.field(at + offset_of!(ClusterDag, pages), |m| &m.cluster_dag.pages),
        s,
    )
    .map_err(err)?;
    let s = b.push_slice(&packed.dependencies);
    b.link_slice(
        root.field(at + offset_of!(ClusterDag, dependencies), |m| {
            &m.cluster_dag.dependencies
        }),
        s,
    )
    .map_err(err)?;
    let s = b.push_slice(&packed.groups);
    b.link_slice(
        root.field(at + offset_of!(ClusterDag, groups), |m| {
            &m.cluster_dag.groups
        }),
        s,
    )
    .map_err(err)?;
    let s = b.push_slice(&packed.parts);
    b.link_slice(
        root.field(at + offset_of!(ClusterDag, parts), |m| &m.cluster_dag.parts),
        s,
    )
    .map_err(err)?;
    let s = b.push_slice(&packed.nodes);
    b.link_slice(
        root.field(at + offset_of!(ClusterDag, nodes), |m| &m.cluster_dag.nodes),
        s,
    )
    .map_err(err)?;
    let sections: Vec<RtSection> = lod
        .sections
        .iter()
        .map(|&(first_index, index_count)| RtSection {
            first_index,
            index_count,
        })
        .collect();
    let s = b.push_slice(&sections);
    b.link_slice(
        root.field(
            at + offset_of!(ClusterDag, rt) + offset_of!(RtLod, sections),
            |m| &m.cluster_dag.rt.sections,
        ),
        s,
    )
    .map_err(err)?;
    Ok(Built {
        payload: b.bytes().to_vec(),
        pages,
        dag_levels: dag.levels,
        dag_clusters: dag.clusters.len(),
    })
}
