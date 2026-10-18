//! A backend without a GPU: imported objects are descriptions, and memory requirements follow a
//! fixed model. Tests build graphs over it and check the compiled plan (0007 §12).

use marlin_rhi::{
    BufferDesc, MSAA_PLACEMENT_ALIGNMENT, MemoryRequirements, PLACEMENT_ALIGNMENT, Result,
    TextureDesc,
};

use crate::backend::{Backend, Queues};

pub enum Recording {}

/// Sizes resources by a fixed model: a texture takes the bytes of its mips and layers rounded up
/// to [`PLACEMENT_ALIGNMENT`] (multisampled ones to [`MSAA_PLACEMENT_ALIGNMENT`]), a buffer its
/// size rounded up to [`Device::BUFFER_ALIGNMENT`]. Says which dedicated queues it has.
#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub compute_queue: bool,
    pub copy_queue: bool,
}

impl Default for Device {
    fn default() -> Self {
        Self::ASYNC
    }
}

impl Device {
    /// Dedicated compute and copy queues.
    pub const ASYNC: Self = Self {
        compute_queue: true,
        copy_queue: true,
    };
    /// The graphics queue only.
    pub const GRAPHICS_ONLY: Self = Self {
        compute_queue: false,
        copy_queue: false,
    };

    pub const BUFFER_ALIGNMENT: u64 = 256;

    pub fn texture_requirements(&self, desc: &TextureDesc) -> MemoryRequirements {
        let extent = desc.format.block_extent();
        let layers = match desc.dimension {
            marlin_rhi::TextureDimension::D2 => desc.array_layers,
            marlin_rhi::TextureDimension::D3 => 1,
        };
        let mut size = 0;
        for mip in 0..desc.mip_levels {
            let (w, h) = desc.mip_size(mip);
            let blocks = u64::from(w.div_ceil(extent)) * u64::from(h.div_ceil(extent));
            size += blocks * u64::from(desc.format.block_size()) * u64::from(desc.mip_depth(mip));
        }
        size *= u64::from(layers) * u64::from(desc.sample_count);
        let alignment = if desc.sample_count > 1 {
            MSAA_PLACEMENT_ALIGNMENT
        } else {
            PLACEMENT_ALIGNMENT
        };
        MemoryRequirements {
            size: size.next_multiple_of(alignment),
            alignment,
        }
    }

    pub fn buffer_requirements(&self, desc: &BufferDesc) -> MemoryRequirements {
        MemoryRequirements {
            size: desc.size.next_multiple_of(Self::BUFFER_ALIGNMENT),
            alignment: Self::BUFFER_ALIGNMENT,
        }
    }
}

/// An imported texture: only its description.
#[derive(Clone, Copy, Debug)]
pub struct Texture(pub TextureDesc);

/// An imported buffer: only its description.
#[derive(Clone, Copy, Debug)]
pub struct Buffer(pub BufferDesc);

impl Backend for Recording {
    type Device = Device;
    type Texture = Texture;
    type Buffer = Buffer;
    type Parts<'a> = core::convert::Infallible;

    fn texture_desc(texture: &Texture) -> &TextureDesc {
        &texture.0
    }

    fn buffer_desc(buffer: &Buffer) -> &BufferDesc {
        &buffer.0
    }

    /// The object's address: keep it in place while a state table knows it.
    fn texture_key(texture: &Texture) -> u64 {
        core::ptr::from_ref(texture) as u64
    }

    fn buffer_key(buffer: &Buffer) -> u64 {
        core::ptr::from_ref(buffer) as u64
    }

    fn texture_requirements(device: &Device, desc: &TextureDesc) -> Result<MemoryRequirements> {
        Ok(device.texture_requirements(desc))
    }

    fn buffer_requirements(device: &Device, desc: &BufferDesc) -> MemoryRequirements {
        device.buffer_requirements(desc)
    }

    fn queues(device: &Device) -> Queues {
        Queues {
            compute: device.compute_queue,
            copy: device.copy_queue,
        }
    }
}
