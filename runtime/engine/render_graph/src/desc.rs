//! Descriptions of the resources the graph creates. They name no usage: the graph derives it
//! from the frame's accesses (0007 §2.2).

use core::hash::{Hash, Hasher};

use marlin_rhi::{BufferUsage, ClearValue, Format, MemoryLocation, TextureDimension, TextureUsage};

/// A transient texture. Like [`marlin_rhi::TextureDesc`] without the usage.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TextureDesc {
    pub dimension: TextureDimension,
    pub width: u32,
    pub height: u32,
    /// Texels deep: 1 for 2D textures.
    pub depth: u32,
    pub array_layers: u32,
    pub mip_levels: u32,
    pub format: Format,
    pub clear_value: Option<ClearValue>,
    pub sample_count: u32,
}

impl TextureDesc {
    /// One layer, one mip, one sample, no clear value.
    pub const fn new_2d(width: u32, height: u32, format: Format) -> Self {
        Self {
            dimension: TextureDimension::D2,
            width,
            height,
            depth: 1,
            array_layers: 1,
            mip_levels: 1,
            format,
            clear_value: None,
            sample_count: 1,
        }
    }

    /// A volume: one layer, one mip.
    pub const fn new_3d(width: u32, height: u32, depth: u32, format: Format) -> Self {
        Self {
            dimension: TextureDimension::D3,
            depth,
            ..Self::new_2d(width, height, format)
        }
    }

    pub const fn with_mips(self, mip_levels: u32) -> Self {
        Self { mip_levels, ..self }
    }

    pub const fn with_layers(self, array_layers: u32) -> Self {
        Self {
            array_layers,
            ..self
        }
    }

    pub const fn with_samples(self, sample_count: u32) -> Self {
        Self {
            sample_count,
            ..self
        }
    }

    pub const fn with_clear(self, clear_value: ClearValue) -> Self {
        Self {
            clear_value: Some(clear_value),
            ..self
        }
    }

    /// Subresources barriers track: mips × layers (a volume's slices are one subresource).
    pub(crate) const fn subresources(&self) -> u32 {
        self.mip_levels * self.layers()
    }

    pub(crate) const fn layers(&self) -> u32 {
        match self.dimension {
            TextureDimension::D2 => self.array_layers,
            TextureDimension::D3 => 1,
        }
    }

    /// The RHI description with the derived `usage`.
    pub const fn to_rhi(&self, usage: TextureUsage) -> marlin_rhi::TextureDesc {
        marlin_rhi::TextureDesc {
            dimension: self.dimension,
            width: self.width,
            height: self.height,
            depth: self.depth,
            array_layers: self.array_layers,
            mip_levels: self.mip_levels,
            format: self.format,
            usage,
            clear_value: self.clear_value,
            sample_count: self.sample_count,
        }
    }

    /// The RHI description without its usage.
    pub const fn from_rhi(desc: &marlin_rhi::TextureDesc) -> Self {
        Self {
            dimension: desc.dimension,
            width: desc.width,
            height: desc.height,
            depth: desc.depth,
            array_layers: desc.array_layers,
            mip_levels: desc.mip_levels,
            format: desc.format,
            clear_value: desc.clear_value,
            sample_count: desc.sample_count,
        }
    }
}

impl Hash for TextureDesc {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.dimension.hash(state);
        (self.width, self.height, self.depth).hash(state);
        (self.array_layers, self.mip_levels, self.sample_count).hash(state);
        self.format.hash(state);
        match self.clear_value {
            None => 0u8.hash(state),
            Some(ClearValue::Color(c)) => {
                1u8.hash(state);
                c.map(f32::to_bits).hash(state);
            }
            Some(ClearValue::Depth(d)) => {
                2u8.hash(state);
                d.to_bits().hash(state);
            }
        }
    }
}

/// A transient buffer in device-local memory. Data the CPU writes each frame goes through the
/// upload ring instead.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BufferDesc {
    /// Bytes.
    pub size: u64,
}

impl BufferDesc {
    pub const fn new(size: u64) -> Self {
        Self { size }
    }

    /// `count` elements of `T`.
    pub const fn structured<T>(count: u64) -> Self {
        Self::new(size_of::<T>() as u64 * count)
    }

    pub const fn to_rhi(&self, usage: BufferUsage) -> marlin_rhi::BufferDesc {
        marlin_rhi::BufferDesc {
            size: self.size,
            usage,
            memory: MemoryLocation::GpuOnly,
        }
    }
}
