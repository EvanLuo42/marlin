//! Imported geometry to a cooked mesh: weld, LOD chain, per-section cache order, meshlets,
//! vertex fetch order, and the streams of `marlin_render::asset::mesh`.

use core::mem::offset_of;

use marlin_render::asset::Mesh;
use marlin_render::asset::mesh::{
    CookedMesh, LodSection, MESHLET_MAX_TRIANGLES, MESHLET_MAX_VERTICES, MeshFlags, MeshJoint,
    MeshLod, MeshSection, Meshlet, PageRange, PositionPrecision, encode_tangent_frame, f32_to_f16,
    position_scale, quantize_position,
};
use marlin_serial::cooked::{CookedBuilder, RelSlice};

use crate::import::Geometry;
use crate::meshlet;
use crate::optimize;
use crate::simplify;

/// What [`build`] made: the payload and one page per LOD.
pub struct Built {
    pub payload: Vec<u8>,
    pub pages: Vec<Vec<u8>>,
    /// Triangles per LOD, finest first.
    pub lod_triangles: Vec<usize>,
    pub lod_errors: Vec<f32>,
}

/// A LOD stops the chain when it removes less than this fraction of the previous one's
/// triangles: the simplifier is stuck against locked borders or the error limit.
const MIN_REDUCTION: f32 = 0.1;

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

    // LOD chain, each simplified from LOD 0 so errors do not compound.
    let lod_count = settings.lods.count.clamp(1, 16) as usize;
    let max_error = if settings.lods.max_error > 0.0 {
        settings.lods.max_error * radius
    } else {
        f32::MAX
    };
    let mut lods: Vec<(Vec<u32>, Vec<u16>, f32)> =
        vec![(indices.clone(), g.triangle_sections.clone(), 0.0)];
    for _ in 1..lod_count {
        let previous = lods.last().map_or(0, |l| l.0.len() / 3);
        let target = ((previous as f32) * settings.lods.reduction.clamp(0.01, 0.99)) as usize;
        let out = simplify::simplify(
            &simplify::Input {
                positions: &positions,
                attributes: &attributes,
                attribute_count,
                indices: &indices,
                sections: &g.triangle_sections,
            },
            target.max(1),
            max_error,
        );
        let tris = out.indices.len() / 3;
        if tris == 0 || tris as f32 > previous as f32 * (1.0 - MIN_REDUCTION) {
            break;
        }
        lods.push((out.indices, out.sections, out.error));
    }

    let section_count = g.section_names.len();
    let position_float = settings.precision == PositionPrecision::Float;
    let scale = position_scale(lo, hi);
    let position_stride = if position_float { 12 } else { 8 };
    let attribute_stride = 4 + 4 * uv_sets as u32 + if colors.is_some() { 4 } else { 0 };
    let skin_stride = if skinned { 12 } else { 0 };

    let mut pages = Vec::with_capacity(lods.len());
    let mut lod_records: Vec<(MeshLod, Vec<LodSection>)> = Vec::with_capacity(lods.len());
    for (lod_index, (lod_indices, lod_sections, error)) in lods.iter().enumerate() {
        // Per section: cache order, then meshlets.
        let mut by_section: Vec<Vec<u32>> = vec![Vec::new(); section_count];
        for (t, &s) in lod_sections.iter().enumerate() {
            by_section[s as usize].extend_from_slice(&lod_indices[t * 3..t * 3 + 3]);
        }
        let mut all_meshlets: Vec<(meshlet::Built, u16)> = Vec::new();
        let mut meshlet_vertices: Vec<u32> = Vec::new();
        let mut meshlet_triangles: Vec<u8> = Vec::new();
        let mut ordered: Vec<u32> = Vec::with_capacity(lod_indices.len());
        let mut sections = Vec::with_capacity(section_count);
        for (s, tris) in by_section.iter().enumerate() {
            let cached = optimize::optimize_vertex_cache(tris, unique);
            let built = meshlet::build(
                &cached,
                &positions,
                MESHLET_MAX_VERTICES,
                MESHLET_MAX_TRIANGLES,
            );
            sections.push(LodSection {
                first_meshlet: all_meshlets.len() as u32,
                meshlet_count: built.meshlets.len() as u32,
                first_index: ordered.len() as u32,
                index_count: (built.triangles.len()) as u32,
            });
            for m in &built.meshlets {
                let verts = &built.vertices[m.vertex_offset as usize..][..m.vertex_count as usize];
                let local =
                    &built.triangles[m.triangle_offset as usize..][..m.triangle_count as usize * 3];
                // The index list follows meshlet order: the same locality for non-mesh-shader
                // paths.
                ordered.extend(local.iter().map(|&l| verts[l as usize]));
                all_meshlets.push((
                    meshlet::Built {
                        vertex_offset: meshlet_vertices.len() as u32,
                        vertex_count: m.vertex_count,
                        triangle_offset: meshlet_triangles.len() as u32,
                        triangle_count: m.triangle_count,
                    },
                    s as u16,
                ));
                meshlet_vertices.extend_from_slice(verts);
                meshlet_triangles.extend_from_slice(local);
            }
        }

        // Vertex fetch order: renumber by first use; LOD-local vertex streams.
        let (lod_ordered, old_of) = optimize::optimize_vertex_fetch(&ordered, unique);
        let mut new_of = vec![u32::MAX; unique];
        for (new, &old) in old_of.iter().enumerate() {
            new_of[old as usize] = new as u32;
        }
        for v in &mut meshlet_vertices {
            *v = new_of[*v as usize];
        }
        let lod_positions: Vec<[f32; 3]> = old_of.iter().map(|&w| positions[w as usize]).collect();
        let vertex_count = old_of.len();

        let mut page: Vec<u8> = Vec::new();
        let range = |page: &mut Vec<u8>, write: &dyn Fn(&mut Vec<u8>)| -> PageRange {
            let offset = align16(page);
            write(page);
            PageRange {
                offset,
                size: page.len() as u32 - offset,
            }
        };
        let positions_range = range(&mut page, &|out| {
            for p in &lod_positions {
                if position_float {
                    out.extend(p.iter().flat_map(|c| c.to_le_bytes()));
                } else {
                    let q = quantize_position(*p, lo, scale);
                    out.extend(q.iter().flat_map(|c| c.to_le_bytes()));
                    out.extend_from_slice(&[0, 0]);
                }
            }
        });
        let attributes_range = range(&mut page, &|out| {
            for &w in &old_of {
                let v = source(w as usize);
                let t = g.tangents[v];
                out.extend(
                    encode_tangent_frame(g.normals[v], [t[0], t[1], t[2]], t[3]).to_le_bytes(),
                );
                for set in &g.uvs[..uv_sets] {
                    out.extend(set[v].iter().flat_map(|c| f32_to_f16(*c).to_le_bytes()));
                }
                if let Some(c) = colors {
                    out.extend(c[v].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8));
                }
            }
        });
        let skin_range = range(&mut page, &|out| {
            if skinned {
                for &w in &old_of {
                    let (j, wt) = skin_q[source(w as usize)];
                    out.extend(j.iter().flat_map(|c| c.to_le_bytes()));
                    out.extend_from_slice(&wt);
                }
            }
        });
        let meshlet_records: Vec<Meshlet> = all_meshlets
            .iter()
            .map(|(m, section)| {
                let b = meshlet::bounds(m, &meshlet_vertices, &meshlet_triangles, &lod_positions);
                Meshlet {
                    center: b.center,
                    radius: b.radius,
                    cone: b.cone,
                    vertex_offset: m.vertex_offset,
                    triangle_offset: m.triangle_offset,
                    vertex_count: m.vertex_count as u8,
                    triangle_count: m.triangle_count as u8,
                    section: *section,
                    group: lod_index as u32,
                    lod_error: *error,
                    parent_error: f32::INFINITY,
                    error_sphere: [center[0], center[1], center[2], radius],
                    lod: lod_index as u32,
                }
            })
            .collect();
        let meshlets_range = range(&mut page, &|out| {
            for m in &meshlet_records {
                out.extend(m.center.iter().flat_map(|c| c.to_le_bytes()));
                out.extend(m.radius.to_le_bytes());
                out.extend(m.cone.map(|c| c as u8));
                out.extend(m.vertex_offset.to_le_bytes());
                out.extend(m.triangle_offset.to_le_bytes());
                out.push(m.vertex_count);
                out.push(m.triangle_count);
                out.extend(m.section.to_le_bytes());
                out.extend(m.group.to_le_bytes());
                out.extend(m.lod_error.to_le_bytes());
                out.extend(m.parent_error.to_le_bytes());
                out.extend(m.error_sphere.iter().flat_map(|c| c.to_le_bytes()));
                out.extend(m.lod.to_le_bytes());
            }
        });
        let meshlet_vertices_range = range(&mut page, &|out| {
            out.extend(meshlet_vertices.iter().flat_map(|v| v.to_le_bytes()));
        });
        let meshlet_triangles_range =
            range(&mut page, &|out| out.extend_from_slice(&meshlet_triangles));
        let index_size = if vertex_count <= 1 << 16 { 2 } else { 4 };
        let indices_range = range(&mut page, &|out| {
            for &i in &lod_ordered {
                if index_size == 2 {
                    out.extend((i as u16).to_le_bytes());
                } else {
                    out.extend(i.to_le_bytes());
                }
            }
        });
        align16(&mut page);
        lod_records.push((
            MeshLod {
                bulk: lod_index as u32,
                vertex_count: vertex_count as u32,
                triangle_count: (lod_ordered.len() / 3) as u32,
                meshlet_count: meshlet_records.len() as u32,
                error: *error,
                index_size,
                positions: positions_range,
                attributes: attributes_range,
                skin: skin_range,
                meshlets: meshlets_range,
                meshlet_vertices: meshlet_vertices_range,
                meshlet_triangles: meshlet_triangles_range,
                indices: indices_range,
                sections: RelSlice::EMPTY,
            },
            sections,
        ));
        pages.push(page);
    }

    // The payload.
    let mut b = CookedBuilder::new();
    let rt_lod = settings
        .rt_lod
        .unwrap_or(1)
        .min(lod_records.len() as u32 - 1);
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
        rt_lod,
        sections: RelSlice::EMPTY,
        lods: RelSlice::EMPTY,
        joints: RelSlice::EMPTY,
    });
    let err = |e: marlin_serial::cooked::LayoutError| e.to_string();
    let section_records = b.push_slice(&vec![
        MeshSection {
            name: RelSlice::EMPTY
        };
        section_count
    ]);
    let lods_pos = b.push_slice(&lod_records.iter().map(|l| l.0).collect::<Vec<_>>());
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
        root.field(offset_of!(CookedMesh, lods), |m| &m.lods),
        lods_pos,
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
    for (i, (_, sections)) in lod_records.iter().enumerate() {
        let s = b.push_slice(sections);

        b.link_slice(
            lods_pos
                .at(i)
                .field(offset_of!(MeshLod, sections), |l| &l.sections),
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
    Ok(Built {
        payload: b.bytes().to_vec(),
        pages,
        lod_triangles: lods.iter().map(|l| l.0.len() / 3).collect(),
        lod_errors: lods.iter().map(|l| l.2).collect(),
    })
}
