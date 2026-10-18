//! Resident geometry (0009 §3.2, R1's discrete LODs): a cooked mesh's LOD pages on the GPU as
//! they are, every LOD resident. One buffer per mesh, read through one raw view:
//!
//! | at | what |
//! |---|---|
//! | `lod * 64` | one record of 16 words per LOD (`Marlin.Engine.Scene.LodStreams`) |
//! | after the records | each LOD's section table: a `LodSection` (4 words) per mesh section |
//! | after the tables, 16-byte aligned | each LOD's page, as cooked |
//!
//! Every offset in a record is bytes from the start of the buffer, so shaders never need the
//! page's own base. The record's words: vertex, triangle and meshlet counts, the LOD's error
//! (float bits), the index size, then the positions, attributes, skin, meshlets, meshlet
//! vertices, meshlet triangles and indices streams, the section table and the section count.
//! The cluster DAG's pages and its RT LOD stay on the CPU until they stream (R2).

use alloc::vec::Vec;

use marlin_rhi::IndexFormat;

use crate::asset::mesh::{MeshError, MeshView, PageRange};

/// Words per LOD record.
pub const LOD_WORDS: usize = 16;

/// What a draw of one LOD needs on the CPU.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct LodDraw {
    /// Byte offset of the LOD's index list in the buffer.
    pub indices: u64,
    pub index_count: u32,
    pub format: IndexFormat,
    /// Object-space simplification error (a distance).
    pub error: f32,
    pub meshlets: u32,
}

/// The buffer's bytes and each LOD's draw. `pages` are the mesh's bulk blobs (indexed by
/// `MeshLod::bulk`); each LOD's page is checked here, since the GPU reads it unchecked.
pub(crate) fn build(
    mesh: &MeshView<'_>,
    pages: &[&[u8]],
) -> Result<(Vec<u8>, Vec<LodDraw>), MeshError> {
    let sections = mesh.sections.len();
    let records = mesh.lods.len() * LOD_WORDS * 4;
    let tables = mesh.lods.len() * sections * 16;
    let mut bytes = alloc::vec![0; (records + tables).next_multiple_of(16)];
    let mut draws = Vec::with_capacity(mesh.lods.len());
    for (i, lod) in mesh.lods.iter().enumerate() {
        let page = *pages.get(lod.bulk as usize).ok_or(MeshError::Malformed)?;
        mesh.check_page(i, page)?;
        let base = bytes.len() as u32;
        bytes.extend_from_slice(page);
        bytes.resize(bytes.len().next_multiple_of(16), 0);
        let at = |r: &PageRange| base + r.offset;
        let table = (records + i * sections * 16) as u32;
        let record: [u32; LOD_WORDS] = [
            lod.vertex_count,
            lod.triangle_count,
            lod.meshlet_count,
            lod.error.to_bits(),
            lod.index_size,
            at(&lod.positions),
            at(&lod.attributes),
            at(&lod.skin),
            at(&lod.meshlets),
            at(&lod.meshlet_vertices),
            at(&lod.meshlet_triangles),
            at(&lod.indices),
            table,
            sections as u32,
            0,
            0,
        ];
        let words = |out: &mut [u8], w: &[u32]| {
            for (b, w) in out.as_chunks_mut::<4>().0.iter_mut().zip(w) {
                *b = w.to_le_bytes();
            }
        };
        words(&mut bytes[i * LOD_WORDS * 4..][..LOD_WORDS * 4], &record);
        for (s, section) in mesh.lod_sections(i).iter().enumerate() {
            let w = [
                section.first_meshlet,
                section.meshlet_count,
                section.first_index,
                section.index_count,
            ];
            words(&mut bytes[table as usize + s * 16..][..16], &w);
        }
        draws.push(LodDraw {
            indices: u64::from(at(&lod.indices)),
            index_count: lod.triangle_count * 3,
            format: if lod.index_size == 2 {
                IndexFormat::U16
            } else {
                IndexFormat::U32
            },
            error: lod.error,
            meshlets: lod.meshlet_count,
        });
    }
    Ok((bytes, draws))
}
