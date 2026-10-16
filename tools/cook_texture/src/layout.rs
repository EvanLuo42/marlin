//! Staging-buffer layout for uploading an [`EncodedTexture`] with buffer-to-texture copies.

use crate::encode::EncodedTexture;

/// The copy alignment rules of the target GPU API.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct UploadAlignment {
    /// Row pitch multiple (`marlin_rhi::TEXTURE_ROW_PITCH_ALIGNMENT`).
    pub row_pitch: u32,
    /// Subresource offset multiple (`marlin_rhi::TEXTURE_OFFSET_ALIGNMENT`).
    pub offset: u64,
}

impl UploadAlignment {
    /// D3D12's, which marlin-rhi requires on every backend.
    pub const RHI: Self = Self {
        row_pitch: 256,
        offset: 512,
    };
}

/// One buffer-to-texture copy, field for field a `marlin_rhi::BufferTextureCopy` at `x = y = 0`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct UploadRegion {
    pub buffer_offset: u64,
    pub bytes_per_row: u32,
    pub mip: u32,
    pub layer: u32,
    /// The mip's size rounded up to whole blocks: block-compressed copies cover whole blocks,
    /// even for mips smaller than one.
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UploadLayout {
    /// Staging bytes needed.
    pub size: u64,
    /// In [`EncodedTexture::subresources`] order.
    pub regions: Vec<UploadRegion>,
}

impl EncodedTexture {
    /// Where each subresource goes in a staging buffer so it can be copied as is.
    pub fn upload_layout(&self, alignment: UploadAlignment) -> UploadLayout {
        let extent = self.format.block_extent();
        let mut size = 0u64;
        let regions = self
            .subresources
            .iter()
            .map(|s| {
                let buffer_offset = size.next_multiple_of(alignment.offset);
                let bytes_per_row = (s.row_pitch as u32).next_multiple_of(alignment.row_pitch);
                size = buffer_offset + bytes_per_row as u64 * s.rows as u64;
                UploadRegion {
                    buffer_offset,
                    bytes_per_row,
                    mip: s.mip,
                    layer: s.layer,
                    width: s.width.next_multiple_of(extent),
                    height: s.height.next_multiple_of(extent),
                }
            })
            .collect();
        UploadLayout { size, regions }
    }

    /// Fills `out` (at least `layout.size` bytes) with the data laid out as `layout` says.
    /// Padding is left untouched.
    pub fn write_upload(&self, layout: &UploadLayout, out: &mut [u8]) {
        assert!(out.len() as u64 >= layout.size, "staging too small");
        for (s, r) in self.subresources.iter().zip(&layout.regions) {
            let src = &self.data[s.offset..][..s.size()];
            for (row, line) in src.chunks_exact(s.row_pitch).enumerate() {
                let at = r.buffer_offset as usize + row * r.bytes_per_row as usize;
                out[at..][..line.len()].copy_from_slice(line);
            }
        }
    }
}
