//! Backend-neutral descriptions, shared by the public API and every backend.

use core::ffi::c_void;
use core::ops::{BitOr, BitOrAssign};
use core::ptr::NonNull;

use crate::{Buffer, DepthStencilView, RenderTargetView, Texture};

macro_rules! flags {
    ($(#[$meta:meta])* $name:ident { $($(#[$fmeta:meta])* $flag:ident = $value:expr,)* }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
        pub struct $name(u32);

        impl $name {
            $($(#[$fmeta])* pub const $flag: Self = Self($value);)*

            #[inline]
            pub const fn bits(self) -> u32 {
                self.0
            }

            #[inline]
            pub const fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }

            #[inline]
            pub const fn union(self, other: Self) -> Self {
                Self(self.0 | other.0)
            }

            /// Any flag of `other` is set.
            #[inline]
            pub const fn intersects(self, other: Self) -> bool {
                self.0 & other.0 != 0
            }

            #[inline]
            pub const fn is_empty(self) -> bool {
                self.0 == 0
            }
        }

        impl BitOr for $name {
            type Output = Self;
            #[inline]
            fn bitor(self, rhs: Self) -> Self {
                Self(self.0 | rhs.0)
            }
        }

        impl BitOrAssign for $name {
            #[inline]
            fn bitor_assign(&mut self, rhs: Self) {
                self.0 |= rhs.0;
            }
        }
    };
}

// --- Instance and adapters ---

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InstanceDesc {
    /// Enables the API's validation layer (D3D12 debug layer, Vulkan validation) when installed.
    /// Its messages go to the `marlin_diag` log. On D3D12 this is process-wide and permanent: the
    /// first instance that enables it enables it for every later device.
    pub validation: bool,
    /// GPU-based validation on top of `validation` (D3D12 GPU-based validation, Vulkan GPU-AV):
    /// checks descriptor indices and resource states as shaders run. Several times slower; for
    /// hunting bindless and barrier bugs. Process-wide and permanent like `validation`.
    pub gpu_validation: bool,
    /// Records what the GPU was doing so that a lost device reports the command lists, the last
    /// operations reached and any page fault (D3D12 DRED auto-breadcrumbs and page-fault
    /// tracking) in the `marlin_diag` log. A few percent of GPU time; on by default so crashes in
    /// the field are diagnosable. Process-wide: it must be set before the first device exists.
    pub crash_diagnostics: bool,
}

impl Default for InstanceDesc {
    fn default() -> Self {
        Self {
            validation: cfg!(debug_assertions),
            gpu_validation: false,
            crash_diagnostics: true,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AdapterKind {
    /// A GPU (discrete or integrated; D3D12 does not say which).
    Hardware,
    /// A CPU rasterizer (WARP, lavapipe). Slow, but always present: for tests and fallback.
    Software,
}

const ADAPTER_NAME_CAPACITY: usize = 128;

/// One adapter as the instance enumerates it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AdapterInfo {
    /// Position in [`Instance::adapters`](crate::Instance::adapters); highest performance first.
    pub index: u32,
    pub kind: AdapterKind,
    /// PCI vendor id: 0x10DE NVIDIA, 0x1002 AMD, 0x8086 Intel, 0x1414 Microsoft (WARP).
    pub vendor_id: u32,
    pub device_id: u32,
    pub dedicated_video_memory: u64,
    /// System memory the adapter may use (D3D12 "shared system memory").
    pub shared_system_memory: u64,
    /// The OS's locally unique adapter id: what OpenXR and other APIs name the adapter by
    /// (DXGI `AdapterLuid`, Vulkan `deviceLUID`), low part in the low 32 bits.
    pub luid: u64,
    name: [u8; ADAPTER_NAME_CAPACITY],
    name_len: u8,
}

impl AdapterInfo {
    #[cfg_attr(
        any(marlin_platform = "linux", marlin_platform = "macos"),
        expect(dead_code, reason = "the stub backend has no adapters")
    )]
    pub(crate) fn new(index: u32, kind: AdapterKind, name: &str) -> Self {
        let mut info = Self {
            index,
            kind,
            vendor_id: 0,
            device_id: 0,
            dedicated_video_memory: 0,
            shared_system_memory: 0,
            luid: 0,
            name: [0; ADAPTER_NAME_CAPACITY],
            name_len: 0,
        };
        let mut len = 0;
        for c in name.chars() {
            if len + c.len_utf8() > ADAPTER_NAME_CAPACITY {
                break;
            }
            len += c.encode_utf8(&mut info.name[len..]).len();
        }
        info.name_len = len as u8;
        info
    }

    pub fn name(&self) -> &str {
        // SAFETY: `new` encodes whole chars only.
        unsafe { core::str::from_utf8_unchecked(&self.name[..self.name_len as usize]) }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AdapterPreference {
    /// The highest-performance hardware adapter that supports the RHI's baseline. Fails with
    /// `NotFound` when there is none; fall back to [`Software`](Self::Software) if acceptable.
    #[default]
    HighPerformance,
    Software,
    /// [`AdapterInfo::index`].
    Index(u32),
    /// The adapter with this [`AdapterInfo::luid`] (the one an XR runtime asks for).
    Luid(u64),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DeviceDesc {
    pub adapter: AdapterPreference,
    /// Use the legacy barrier path even where enhanced barriers exist. For testing the fallback.
    pub disable_enhanced_barriers: bool,
}

/// How far variable-rate shading goes ([`CommandList::set_shading_rate`]).
///
/// [`CommandList::set_shading_rate`]: crate::CommandList::set_shading_rate
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum VariableRateShading {
    #[default]
    None,
    /// One rate per draw (D3D12 VRS tier 1).
    PerDraw,
    /// Per draw, per primitive (`SV_ShadingRate`) and per screen tile from a shading-rate image
    /// (tier 2).
    Image,
}

/// Optional capabilities of a device, beyond the baseline (D3D12: feature level 12_0, shader
/// model 6.6, resource binding tier 3).
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Features {
    /// Highest shader model as `(major, minor)`; at least 6.6 on D3D12.
    pub shader_model: (u8, u8),
    /// Barriers run natively as sync/access/layout (D3D12 enhanced barriers, Vulkan
    /// synchronization2). Without it, D3D12 translates them into legacy resource states.
    pub enhanced_barriers: bool,
    /// [`PresentMode::Immediate`] can tear (variable refresh displays).
    pub tearing: bool,
    /// Mesh and amplification shaders. Part of the baseline: devices without them are refused.
    pub mesh_shader: bool,
    /// Inline ray queries (DXR 1.1) against [`AccelerationStructure`]s, from any shader stage.
    ///
    /// [`AccelerationStructure`]: crate::AccelerationStructure
    pub ray_query: bool,
    /// 16-bit float and integer shader arithmetic (`float16_t`, `int16_t`).
    pub shader_float16_int16: bool,
    /// 64-bit integer shader arithmetic.
    pub shader_int64: bool,
    /// Pixel shaders read barycentric coordinates (`SV_Barycentrics`).
    pub barycentrics: bool,
    /// [`SamplerReduction::Minimum`] / [`Maximum`](SamplerReduction::Maximum) filtering (depth
    /// pyramids, conservative min/max downsampling).
    pub sampler_min_max: bool,
    /// [`RasterState::conservative`]: every pixel a primitive touches is shaded (voxelization,
    /// tiled light binning).
    pub conservative_rasterization: bool,
    /// [`DepthState::bounds_test`] and
    /// [`CommandList::set_depth_bounds`](crate::CommandList::set_depth_bounds).
    pub depth_bounds: bool,
    pub variable_rate_shading: VariableRateShading,
    /// Pixels per side of one shading-rate image texel (8, 16 or 32), with
    /// [`VariableRateShading::Image`]; 0 otherwise.
    pub shading_rate_tile_size: u32,
    /// [`QueryKind::PipelineStatistics`], mesh and amplification counts included.
    pub pipeline_statistics: bool,
    /// [`CommandList::write_marker`](crate::CommandList::write_marker) on every queue type.
    pub buffer_markers: bool,
    /// Sparse buffers and 2D textures ([`Device::create_sparse_texture`]): D3D12 tiled resources
    /// tier 2 (unmapped tiles read zero, shaders can test residency).
    ///
    /// [`Device::create_sparse_texture`]: crate::Device::create_sparse_texture
    pub sparse_resources: bool,
    /// Sparse volume textures (tiled resources tier 3).
    pub sparse_volumes: bool,
    /// [`MemoryLocation::GpuUpload`]: CPU-writable device-local memory (resizable BAR).
    pub gpu_upload_heap: bool,
    /// 64-bit atomics on typed resources and in shared memory (software rasterization).
    pub atomic_int64: bool,
    /// Timestamps can be written on the copy queue.
    pub copy_queue_timestamps: bool,
    /// Heaps may hold buffers and every kind of texture together (D3D12 resource heap tier 2).
    /// Otherwise each heap takes one [`HeapKind`].
    pub mixed_heaps: bool,
    /// The smallest and largest wave (subgroup) sizes, for shaders that pick one with
    /// `[WaveSize]`.
    pub wave_size_range: (u32, u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum QueueType {
    Graphics,
    Compute,
    Copy,
}

// --- Formats and textures ---

/// Texel formats. Component order is memory order (lowest address first).
///
/// Not every format exists on every API: ETC2/EAC and ASTC are mobile and console formats that
/// D3D12 lacks. [`Device::format_support`](crate::Device::format_support) says what a device does
/// with each; creating a texture in a format the device lacks fails with `Unsupported`.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Format {
    R8Unorm,
    R8Snorm,
    R8Uint,
    R8Sint,
    Rg8Unorm,
    Rg8Snorm,
    Rg8Uint,
    Rg8Sint,
    Rgba8Unorm,
    Rgba8UnormSrgb,
    Rgba8Snorm,
    Rgba8Uint,
    Rgba8Sint,
    Bgra8Unorm,
    Bgra8UnormSrgb,
    Rgb10a2Unorm,
    Rgb10a2Uint,
    Rg11b10Float,
    /// Three 9-bit mantissas sharing a 5-bit exponent: HDR colors in 32 bits (read-only).
    Rgb9e5Ufloat,
    R16Unorm,
    R16Snorm,
    R16Uint,
    R16Sint,
    R16Float,
    Rg16Unorm,
    Rg16Snorm,
    Rg16Uint,
    Rg16Sint,
    Rg16Float,
    Rgba16Unorm,
    Rgba16Snorm,
    Rgba16Uint,
    Rgba16Sint,
    Rgba16Float,
    R32Uint,
    R32Sint,
    R32Float,
    /// The visibility buffer: cluster and triangle ids.
    Rg32Uint,
    Rg32Sint,
    Rg32Float,
    Rgba32Uint,
    Rgba32Sint,
    Rgba32Float,
    /// 16-bit depth: shadow maps that need the bandwidth more than the precision.
    D16Unorm,
    /// The depth format: reverse-Z wants float precision.
    D32Float,
    /// Depth with an 8-bit stencil.
    D32FloatS8,
    Bc1Unorm,
    Bc1UnormSrgb,
    Bc2Unorm,
    Bc2UnormSrgb,
    Bc3Unorm,
    Bc3UnormSrgb,
    Bc4Unorm,
    Bc4Snorm,
    Bc5Unorm,
    Bc5Snorm,
    Bc6hUfloat,
    Bc6hSfloat,
    Bc7Unorm,
    Bc7UnormSrgb,
    Etc2Rgb8Unorm,
    Etc2Rgb8UnormSrgb,
    Etc2Rgb8A1Unorm,
    Etc2Rgb8A1UnormSrgb,
    Etc2Rgba8Unorm,
    Etc2Rgba8UnormSrgb,
    EacR11Unorm,
    EacR11Snorm,
    EacRg11Unorm,
    EacRg11Snorm,
    /// ASTC with square blocks (the sizes engines use); the block edge is in the name.
    Astc4x4Unorm,
    Astc4x4UnormSrgb,
    Astc5x5Unorm,
    Astc5x5UnormSrgb,
    Astc6x6Unorm,
    Astc6x6UnormSrgb,
    Astc8x8Unorm,
    Astc8x8UnormSrgb,
    Astc10x10Unorm,
    Astc10x10UnormSrgb,
    Astc12x12Unorm,
    Astc12x12UnormSrgb,
}

impl Format {
    /// `(linear, Some(srgb))` for formats with an sRGB twin, else `(self, None)`.
    const fn srgb_pair(self) -> (Self, Option<Self>) {
        use Format::*;
        let (linear, srgb) = match self {
            Rgba8Unorm | Rgba8UnormSrgb => (Rgba8Unorm, Rgba8UnormSrgb),
            Bgra8Unorm | Bgra8UnormSrgb => (Bgra8Unorm, Bgra8UnormSrgb),
            Bc1Unorm | Bc1UnormSrgb => (Bc1Unorm, Bc1UnormSrgb),
            Bc2Unorm | Bc2UnormSrgb => (Bc2Unorm, Bc2UnormSrgb),
            Bc3Unorm | Bc3UnormSrgb => (Bc3Unorm, Bc3UnormSrgb),
            Bc7Unorm | Bc7UnormSrgb => (Bc7Unorm, Bc7UnormSrgb),
            Etc2Rgb8Unorm | Etc2Rgb8UnormSrgb => (Etc2Rgb8Unorm, Etc2Rgb8UnormSrgb),
            Etc2Rgb8A1Unorm | Etc2Rgb8A1UnormSrgb => (Etc2Rgb8A1Unorm, Etc2Rgb8A1UnormSrgb),
            Etc2Rgba8Unorm | Etc2Rgba8UnormSrgb => (Etc2Rgba8Unorm, Etc2Rgba8UnormSrgb),
            Astc4x4Unorm | Astc4x4UnormSrgb => (Astc4x4Unorm, Astc4x4UnormSrgb),
            Astc5x5Unorm | Astc5x5UnormSrgb => (Astc5x5Unorm, Astc5x5UnormSrgb),
            Astc6x6Unorm | Astc6x6UnormSrgb => (Astc6x6Unorm, Astc6x6UnormSrgb),
            Astc8x8Unorm | Astc8x8UnormSrgb => (Astc8x8Unorm, Astc8x8UnormSrgb),
            Astc10x10Unorm | Astc10x10UnormSrgb => (Astc10x10Unorm, Astc10x10UnormSrgb),
            Astc12x12Unorm | Astc12x12UnormSrgb => (Astc12x12Unorm, Astc12x12UnormSrgb),
            other => return (other, None),
        };
        (linear, Some(srgb))
    }

    pub const fn is_srgb(self) -> bool {
        match self.srgb_pair() {
            (_, Some(srgb)) => srgb as u32 == self as u32,
            _ => false,
        }
    }

    /// The same layout without sRGB encoding.
    pub const fn linear(self) -> Self {
        self.srgb_pair().0
    }

    /// The same layout with sRGB encoding, if it has one.
    pub const fn srgb(self) -> Option<Self> {
        self.srgb_pair().1
    }

    pub const fn is_depth(self) -> bool {
        matches!(self, Self::D16Unorm | Self::D32Float | Self::D32FloatS8)
    }

    pub const fn has_stencil(self) -> bool {
        matches!(self, Self::D32FloatS8)
    }

    /// Integer formats: read and written as integers, never filtered or blended.
    pub const fn is_integer(self) -> bool {
        self.is_unsigned_integer() || self.is_signed_integer()
    }

    pub const fn is_unsigned_integer(self) -> bool {
        use Format::*;
        matches!(
            self,
            R8Uint
                | Rg8Uint
                | Rgba8Uint
                | Rgb10a2Uint
                | R16Uint
                | Rg16Uint
                | Rgba16Uint
                | R32Uint
                | Rg32Uint
                | Rgba32Uint
        )
    }

    pub const fn is_signed_integer(self) -> bool {
        use Format::*;
        matches!(
            self,
            R8Sint
                | Rg8Sint
                | Rgba8Sint
                | R16Sint
                | Rg16Sint
                | Rgba16Sint
                | R32Sint
                | Rg32Sint
                | Rgba32Sint
        )
    }

    /// Block-compressed (BCn, ETC2/EAC, ASTC).
    pub const fn is_compressed(self) -> bool {
        self.block_extent() > 1
    }

    /// Width and height of a block in texels: 4 for BCn and ETC2/EAC, the ASTC block edge, else
    /// 1.
    pub const fn block_extent(self) -> u32 {
        use Format::*;
        match self {
            Bc1Unorm | Bc1UnormSrgb | Bc2Unorm | Bc2UnormSrgb | Bc3Unorm | Bc3UnormSrgb
            | Bc4Unorm | Bc4Snorm | Bc5Unorm | Bc5Snorm | Bc6hUfloat | Bc6hSfloat | Bc7Unorm
            | Bc7UnormSrgb | Etc2Rgb8Unorm | Etc2Rgb8UnormSrgb | Etc2Rgb8A1Unorm
            | Etc2Rgb8A1UnormSrgb | Etc2Rgba8Unorm | Etc2Rgba8UnormSrgb | EacR11Unorm
            | EacR11Snorm | EacRg11Unorm | EacRg11Snorm | Astc4x4Unorm | Astc4x4UnormSrgb => 4,
            Astc5x5Unorm | Astc5x5UnormSrgb => 5,
            Astc6x6Unorm | Astc6x6UnormSrgb => 6,
            Astc8x8Unorm | Astc8x8UnormSrgb => 8,
            Astc10x10Unorm | Astc10x10UnormSrgb => 10,
            Astc12x12Unorm | Astc12x12UnormSrgb => 12,
            _ => 1,
        }
    }

    /// Bytes per block in buffer↔texture copies: [`block_size`](Self::block_size), except that
    /// depth-stencil formats copy only their depth plane.
    pub const fn copy_block_size(self) -> u32 {
        match self {
            Self::D32FloatS8 => 4,
            other => other.block_size(),
        }
    }

    /// Bytes per block (per texel for uncompressed formats).
    pub const fn block_size(self) -> u32 {
        use Format::*;
        match self {
            R8Unorm | R8Snorm | R8Uint | R8Sint => 1,
            Rg8Unorm | Rg8Snorm | Rg8Uint | Rg8Sint | R16Unorm | R16Snorm | R16Uint | R16Sint
            | R16Float | D16Unorm => 2,
            Rgba8Unorm | Rgba8UnormSrgb | Rgba8Snorm | Rgba8Uint | Rgba8Sint | Bgra8Unorm
            | Bgra8UnormSrgb | Rgb10a2Unorm | Rgb10a2Uint | Rg11b10Float | Rgb9e5Ufloat
            | Rg16Unorm | Rg16Snorm | Rg16Uint | Rg16Sint | Rg16Float | R32Uint | R32Sint
            | R32Float | D32Float => 4,
            Rgba16Unorm | Rgba16Snorm | Rgba16Uint | Rgba16Sint | Rgba16Float | Rg32Uint
            | Rg32Sint | Rg32Float | D32FloatS8 => 8,
            Rgba32Uint | Rgba32Sint | Rgba32Float => 16,
            Bc1Unorm | Bc1UnormSrgb | Bc4Unorm | Bc4Snorm | Etc2Rgb8Unorm | Etc2Rgb8UnormSrgb
            | Etc2Rgb8A1Unorm | Etc2Rgb8A1UnormSrgb | EacR11Unorm | EacR11Snorm => 8,
            Bc2Unorm | Bc2UnormSrgb | Bc3Unorm | Bc3UnormSrgb | Bc5Unorm | Bc5Snorm
            | Bc6hUfloat | Bc6hSfloat | Bc7Unorm | Bc7UnormSrgb | Etc2Rgba8Unorm
            | Etc2Rgba8UnormSrgb | EacRg11Unorm | EacRg11Snorm | Astc4x4Unorm
            | Astc4x4UnormSrgb | Astc5x5Unorm | Astc5x5UnormSrgb | Astc6x6Unorm
            | Astc6x6UnormSrgb | Astc8x8Unorm | Astc8x8UnormSrgb | Astc10x10Unorm
            | Astc10x10UnormSrgb | Astc12x12Unorm | Astc12x12UnormSrgb => 16,
        }
    }
}

flags! {
    /// What a device can do with a [`Format`] ([`Device::format_support`]).
    ///
    /// [`Device::format_support`]: crate::Device::format_support
    FormatFeatures {
        /// Read-only shader views (`Load`).
        SAMPLED = 0x1,
        /// Linear filtering when sampled.
        FILTER = 0x2,
        /// Comparison sampling (shadow maps).
        COMPARISON = 0x4,
        /// Writable shader views of this exact format.
        STORAGE_WRITE = 0x8,
        /// Loads through writable shader views (typed UAV loads).
        STORAGE_READ = 0x10,
        /// 32-bit atomics through writable views.
        STORAGE_ATOMIC = 0x20,
        RENDER_TARGET = 0x40,
        /// Blending as a render target.
        BLEND = 0x80,
        DEPTH_STENCIL = 0x100,
        /// Source and destination of
        /// [`CommandList::resolve_texture`](crate::CommandList::resolve_texture).
        RESOLVE = 0x200,
        /// Multisampled textures can be read by shaders (`Texture2DMS`).
        MULTISAMPLE_LOAD = 0x400,
        /// Sparse textures ([`Device::create_sparse_texture`](crate::Device::create_sparse_texture)).
        SPARSE = 0x800,
    }
}

/// [`Device::format_support`](crate::Device::format_support): the format's features and the
/// sample counts it renders at.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct FormatSupport {
    pub features: FormatFeatures,
    /// Bit `n` set: `n` samples per pixel work as a render target or depth texture. Bit 1 is
    /// set for every render target and depth format; 0 for formats that are neither.
    pub sample_counts: u32,
}

impl FormatSupport {
    pub const fn supports_samples(&self, count: u32) -> bool {
        count < 32 && self.sample_counts & (1 << count) != 0
    }
}

/// Row pitch alignment of buffer↔texture copies (D3D12's; the strictest of the APIs).
pub const TEXTURE_ROW_PITCH_ALIGNMENT: u32 = 256;
/// Buffer offset alignment of buffer↔texture copies.
pub const TEXTURE_OFFSET_ALIGNMENT: u64 = 512;
/// Offset and size alignment of uniform buffer views.
pub const UNIFORM_ALIGNMENT: u64 = 256;
/// Largest uniform buffer view.
pub const MAX_UNIFORM_SIZE: u64 = 65536;
/// 32-bit words of push constants (D3D12 root constants, Vulkan push constants).
pub const MAX_PUSH_CONSTANTS: u32 = 32;
/// Slots in the bindless resource table: buffer and texture views, see [`ShaderView`].
///
/// [`ShaderView`]: crate::ShaderView
pub const MAX_BINDLESS_RESOURCES: u32 = 1 << 18;
/// Slots in the bindless sampler table (D3D12's shader-visible sampler heap limit).
pub const MAX_BINDLESS_SAMPLERS: u32 = 2048;

// --- Memory, buffers and textures ---

/// Where a resource's memory lives.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum MemoryLocation {
    /// Device-local. Filled by copies and shaders; the fastest for the GPU.
    #[default]
    GpuOnly,
    /// CPU-written, GPU-read (write-combined: never read it from the CPU). Staging and per-frame
    /// data. Mapped for the buffer's life.
    Upload,
    /// GPU-written, CPU-read: results to read back. Mapped for the buffer's life. Survives a lost
    /// device, so crash markers ([`CommandList::write_marker`](crate::CommandList::write_marker))
    /// go here.
    Readback,
    /// Device-local memory the CPU writes directly (resizable BAR; D3D12 GPU upload heaps):
    /// per-frame data the GPU reads at full speed without a staging copy. Write-combined like
    /// [`Upload`](Self::Upload). Requires [`Features::gpu_upload_heap`]; buffers only.
    GpuUpload,
}

impl MemoryLocation {
    /// The CPU can write (and for readback, read) it through [`Buffer::mapped`].
    ///
    /// [`Buffer::mapped`]: crate::Buffer::mapped
    pub const fn is_mapped(self) -> bool {
        !matches!(self, Self::GpuOnly)
    }
}

/// A linear GPU buffer. `size` is in bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BufferDesc {
    pub size: u64,
    pub usage: BufferUsage,
    pub memory: MemoryLocation,
}

/// A texture's clear value the hardware is optimized for (D3D12 "optimized clear value"; other
/// APIs ignore it). Clearing to any other value works, only slower.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ClearValue {
    Color([f32; 4]),
    Depth(f32),
}

/// A texture's shape.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum TextureDimension {
    /// 2D, with `array_layers` layers (cube maps are 6-layer 2D textures).
    #[default]
    D2,
    /// A volume `depth` texels deep; one layer. Froxels, noise, LUTs, probe volumes.
    D3,
}

/// A 2D texture, 2D texture array (cube maps are arrays of 6 viewed as cubes) or volume.
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
    pub usage: TextureUsage,
    pub clear_value: Option<ClearValue>,
    /// Samples per pixel: 1, or 2, 4, 8 or 16 for a multisampled 2D render target or depth
    /// texture (one mip, no [`TextureUsage::STORAGE`]). [`Device::format_support`] lists the
    /// counts a format allows.
    ///
    /// [`Device::format_support`]: crate::Device::format_support
    pub sample_count: u32,
}

impl TextureDesc {
    /// One layer, one mip, one sample, no clear value.
    pub const fn new_2d(width: u32, height: u32, format: Format, usage: TextureUsage) -> Self {
        Self {
            dimension: TextureDimension::D2,
            width,
            height,
            depth: 1,
            array_layers: 1,
            mip_levels: 1,
            format,
            usage,
            clear_value: None,
            sample_count: 1,
        }
    }

    /// A volume texture: one layer, one mip, no clear value.
    pub const fn new_3d(
        width: u32,
        height: u32,
        depth: u32,
        format: Format,
        usage: TextureUsage,
    ) -> Self {
        Self {
            dimension: TextureDimension::D3,
            depth,
            ..Self::new_2d(width, height, format, usage)
        }
    }

    /// Size of mip `level` in texels.
    pub const fn mip_size(&self, level: u32) -> (u32, u32) {
        let w = self.width >> level;
        let h = self.height >> level;
        (if w == 0 { 1 } else { w }, if h == 0 { 1 } else { h })
    }

    /// Depth of mip `level` in texels (1 for 2D textures).
    pub const fn mip_depth(&self, level: u32) -> u32 {
        let d = self.depth >> level;
        if d == 0 { 1 } else { d }
    }
}

/// Mip levels and array layers of a texture.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SubresourceRange {
    pub base_mip: u32,
    /// `u32::MAX`: through the last mip.
    pub mip_count: u32,
    pub base_layer: u32,
    /// `u32::MAX`: through the last layer.
    pub layer_count: u32,
}

impl SubresourceRange {
    pub const ALL: Self = Self {
        base_mip: 0,
        mip_count: u32::MAX,
        base_layer: 0,
        layer_count: u32::MAX,
    };
}

flags! {
    /// What a buffer may be used for. Vulkan needs the full set up front; D3D12 only cares about
    /// [`SHADER_WRITE`](Self::SHADER_WRITE).
    BufferUsage {
        COPY_SRC = 0x1,
        COPY_DST = 0x2,
        INDEX = 0x4,
        /// Uniform (constant) buffer views.
        UNIFORM = 0x8,
        /// Read-only structured and raw views.
        SHADER_READ = 0x10,
        /// Writable structured and raw views. [`MemoryLocation::GpuOnly`] only.
        SHADER_WRITE = 0x20,
        /// Indirect arguments and counts, and predicates
        /// ([`CommandList::set_predication`](crate::CommandList::set_predication)).
        INDIRECT = 0x40,
        /// Vertex, index, transform, AABB and instance data that acceleration structure builds
        /// read.
        ACCELERATION_STRUCTURE_INPUT = 0x80,
        /// May be exported to another device or process
        /// ([`Device::export_buffer`](crate::Device::export_buffer)).
        SHARED = 0x100,
        /// Holds acceleration structures, at [`ACCELERATION_STRUCTURE_ALIGNMENT`] offsets
        /// ([`Device::create_acceleration_structure_in`](crate::Device::create_acceleration_structure_in)).
        /// Combines with no other usage; [`MemoryLocation::GpuOnly`], committed or placed. Its
        /// structures are synchronized through barriers on the whole buffer.
        ACCELERATION_STRUCTURE_STORAGE = 0x200,
    }
}

flags! {
    /// What a texture may be used for.
    TextureUsage {
        COPY_SRC = 0x1,
        COPY_DST = 0x2,
        /// Read-only shader views.
        SAMPLED = 0x4,
        /// Writable shader views. Not for depth, sRGB or block-compressed formats.
        STORAGE = 0x8,
        RENDER_TARGET = 0x10,
        /// Depth formats only.
        DEPTH_STENCIL = 0x20,
        /// A shading-rate image ([`CommandList::set_shading_rate_image`]): [`Format::R8Uint`],
        /// one texel per [`Features::shading_rate_tile_size`]² pixels.
        ///
        /// [`CommandList::set_shading_rate_image`]: crate::CommandList::set_shading_rate_image
        SHADING_RATE = 0x40,
        /// May be exported to another device or process
        /// ([`Device::export_texture`](crate::Device::export_texture)).
        SHARED = 0x80,
    }
}

// --- Views and samplers ---

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct RenderTargetViewDesc {
    /// `None`: the texture's format. Otherwise the sRGB/linear variant of it.
    pub format: Option<Format>,
    pub mip: u32,
    pub layer: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct DepthStencilViewDesc {
    pub mip: u32,
    pub layer: u32,
    /// Depth testing without writes, while the texture is also sampled.
    pub read_only: bool,
}

/// What a shader declares the view as. Multisampled textures are viewed as `D2` / `D2Array`
/// and read as `Texture2DMS` / `Texture2DMSArray`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TextureViewDimension {
    /// One layer: layer 0 of the range. D3D12 cannot view a later layer of an array as a plain 2D
    /// texture; use a one-layer `D2Array` for that.
    D2,
    D2Array,
    /// A volume texture. Writable views see every slice of one mip.
    D3,
    /// 6 layers starting at `base_layer`, in +X −X +Y −Y +Z −Z order.
    Cube,
    /// `layer_count / 6` cubes from `base_layer` (point-light shadows, reflection probes).
    CubeArray,
}

/// A bindless view of a texture.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TextureViewDesc {
    /// Writable (requires [`TextureUsage::STORAGE`]; one mip) or read-only (requires
    /// [`TextureUsage::SAMPLED`]).
    pub writable: bool,
    pub dimension: TextureViewDimension,
    /// `None`: the texture's format (depth textures read their depth as `R32Float`, or
    /// `R16Unorm` for [`Format::D16Unorm`]). [`Format::R8Uint`] on a
    /// [`D32FloatS8`](Format::D32FloatS8) texture reads its stencil, in the red channel.
    pub format: Option<Format>,
    pub range: SubresourceRange,
}

impl TextureViewDesc {
    /// Every mip of layer 0, read-only.
    pub const SAMPLED: Self = Self {
        writable: false,
        dimension: TextureViewDimension::D2,
        format: None,
        range: SubresourceRange::ALL,
    };
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BufferViewKind {
    /// A uniform (constant) buffer: `ConstantBuffer<T>`. Offset aligned to [`UNIFORM_ALIGNMENT`];
    /// size at most [`MAX_UNIFORM_SIZE`]. Never writable.
    Uniform,
    /// An array of `stride`-byte elements: `(RW)StructuredBuffer<T>`. Offset and size are
    /// multiples of `stride`.
    Structured { stride: u32 },
    /// 32-bit words: `(RW)ByteAddressBuffer`. Offset and size are multiples of 4.
    Raw,
}

/// A bindless view of a buffer range.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BufferViewDesc {
    pub kind: BufferViewKind,
    /// Requires [`BufferUsage::SHADER_WRITE`].
    pub writable: bool,
    pub offset: u64,
    /// `u64::MAX`: through the end of the buffer.
    pub size: u64,
}

impl BufferViewDesc {
    /// The whole buffer.
    pub const fn whole(kind: BufferViewKind, writable: bool) -> Self {
        Self {
            kind,
            writable,
            offset: 0,
            size: u64::MAX,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Filter {
    Nearest,
    #[default]
    Linear,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum AddressMode {
    #[default]
    Repeat,
    MirrorRepeat,
    ClampToEdge,
    ClampToBorder,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CompareOp {
    Never,
    Less,
    Equal,
    LessEqual,
    Greater,
    NotEqual,
    GreaterEqual,
    Always,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum BorderColor {
    #[default]
    TransparentBlack,
    OpaqueBlack,
    OpaqueWhite,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SamplerDesc {
    pub min_filter: Filter,
    pub mag_filter: Filter,
    pub mip_filter: Filter,
    pub address_u: AddressMode,
    pub address_v: AddressMode,
    pub address_w: AddressMode,
    pub mip_lod_bias: f32,
    /// 1 disables anisotropic filtering; at most 16. Overrides the filters when above 1.
    pub max_anisotropy: u32,
    /// A comparison sampler (shadow maps): `SamplerComparisonState`.
    pub compare: Option<CompareOp>,
    pub min_lod: f32,
    pub max_lod: f32,
    pub border_color: BorderColor,
    /// How filtered texels combine. Not with `compare`.
    pub reduction: SamplerReduction,
}

/// How a filtering sampler combines the texels it reads.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum SamplerReduction {
    /// The weighted average: ordinary filtering.
    #[default]
    WeightedAverage,
    /// The smallest texel in the footprint (requires [`Features::sampler_min_max`]).
    Minimum,
    /// The largest texel in the footprint: conservative depth pyramids under reverse-Z.
    Maximum,
}

impl Default for SamplerDesc {
    /// Trilinear, repeating, every mip.
    fn default() -> Self {
        Self {
            min_filter: Filter::Linear,
            mag_filter: Filter::Linear,
            mip_filter: Filter::Linear,
            address_u: AddressMode::Repeat,
            address_v: AddressMode::Repeat,
            address_w: AddressMode::Repeat,
            mip_lod_bias: 0.0,
            max_anisotropy: 1,
            compare: None,
            min_lod: 0.0,
            max_lod: f32::MAX,
            border_color: BorderColor::TransparentBlack,
            reduction: SamplerReduction::WeightedAverage,
        }
    }
}

// --- Pipelines ---

/// The GPU API the build's backend drives: [`GRAPHICS_API`](crate::GRAPHICS_API). Code that
/// depends on the backend (golden images, API-specific tests) keys on this, never on the platform.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum GraphicsApi {
    D3D12,
    Vulkan,
    Metal,
    /// PlayStation 5.
    Agc,
    /// Nintendo Switch 2.
    Nvn,
}

impl GraphicsApi {
    /// Lowercase name, for directory names and logs: `"vulkan"`, `"d3d12"`, ...
    pub const fn name(self) -> &'static str {
        match self {
            Self::D3D12 => "d3d12",
            Self::Vulkan => "vulkan",
            Self::Metal => "metal",
            Self::Agc => "agc",
            Self::Nvn => "nvn",
        }
    }
}

/// The bytecode the build's backend consumes: [`SHADER_FORMAT`](crate::SHADER_FORMAT).
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ShaderFormat {
    Dxil,
    SpirV,
    MetalLib,
}

/// Compiled shader bytecode: DXIL on D3D12, SPIR-V on Vulkan, the console formats on consoles.
/// The RHI never compiles source; see `docs/design/0002-shader-interface.md` for the interface
/// shaders must follow (bindless tables, push constants, vertex pulling).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ShaderModule<'a> {
    pub code: &'a [u8],
    /// Entry point. SPIR-V modules and DXIL libraries may hold several; D3D12 ignores it for
    /// single-entry DXIL.
    pub entry: &'a str,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum PrimitiveTopology {
    PointList,
    LineList,
    LineStrip,
    #[default]
    TriangleList,
    TriangleStrip,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum CullMode {
    None,
    Front,
    #[default]
    Back,
}

/// Winding of front faces as seen on screen (y up, the math library's convention).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum FrontFace {
    #[default]
    CounterClockwise,
    Clockwise,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum PolygonMode {
    #[default]
    Fill,
    Line,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RasterState {
    pub cull: CullMode,
    pub front_face: FrontFace,
    pub polygon: PolygonMode,
    /// Constant depth bias in units of the depth format's precision. Reverse-Z pushes away with
    /// negative values.
    pub depth_bias: i32,
    pub depth_bias_slope: f32,
    pub depth_bias_clamp: f32,
    /// Clip against the near and far planes (off: clamp depth, for shadow pancaking).
    pub depth_clip: bool,
    /// Overestimating conservative rasterization: every pixel the primitive touches is covered
    /// (requires [`Features::conservative_rasterization`]).
    pub conservative: bool,
}

impl core::hash::Hash for RasterState {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        (self.cull, self.front_face, self.polygon, self.depth_bias).hash(state);
        (
            self.depth_bias_slope.to_bits(),
            self.depth_bias_clamp.to_bits(),
        )
            .hash(state);
        (self.depth_clip, self.conservative).hash(state);
    }
}

impl Default for RasterState {
    fn default() -> Self {
        Self {
            cull: CullMode::Back,
            front_face: FrontFace::CounterClockwise,
            polygon: PolygonMode::Fill,
            depth_bias: 0,
            depth_bias_slope: 0.0,
            depth_bias_clamp: 0.0,
            depth_clip: true,
            conservative: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DepthState {
    pub format: Format,
    pub write: bool,
    pub compare: CompareOp,
    /// Stencil testing; the format must have stencil ([`Format::has_stencil`]).
    pub stencil: Option<StencilState>,
    /// Discards pixels whose stored depth lies outside the range set by
    /// [`CommandList::set_depth_bounds`](crate::CommandList::set_depth_bounds) (light volumes,
    /// requires [`Features::depth_bounds`]).
    pub bounds_test: bool,
}

impl DepthState {
    /// Reverse-Z (near = 1, far = 0) depth testing with writes.
    pub const fn reverse_z(format: Format) -> Self {
        Self {
            format,
            write: true,
            compare: CompareOp::GreaterEqual,
            stencil: None,
            bounds_test: false,
        }
    }
}

/// What a stencil test does to the stored value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum StencilOp {
    #[default]
    Keep,
    Zero,
    /// Writes the reference value
    /// ([`CommandList::set_stencil_reference`](crate::CommandList::set_stencil_reference)).
    Replace,
    IncrementClamp,
    DecrementClamp,
    Invert,
    IncrementWrap,
    DecrementWrap,
}

/// The stencil test of one face: `reference & read_mask` compared with `stored & read_mask`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct StencilFace {
    pub compare: CompareOp,
    /// The stencil test failed.
    pub fail: StencilOp,
    /// The stencil test passed and the depth test failed.
    pub depth_fail: StencilOp,
    /// Both passed.
    pub pass: StencilOp,
}

impl Default for StencilFace {
    fn default() -> Self {
        Self {
            compare: CompareOp::Always,
            fail: StencilOp::Keep,
            depth_fail: StencilOp::Keep,
            pass: StencilOp::Keep,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct StencilState {
    pub read_mask: u8,
    pub write_mask: u8,
    pub front: StencilFace,
    pub back: StencilFace,
}

impl Default for StencilState {
    fn default() -> Self {
        Self {
            read_mask: 0xff,
            write_mask: 0xff,
            front: StencilFace::default(),
            back: StencilFace::default(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BlendFactor {
    Zero,
    One,
    SrcColor,
    OneMinusSrcColor,
    SrcAlpha,
    OneMinusSrcAlpha,
    DstColor,
    OneMinusDstColor,
    DstAlpha,
    OneMinusDstAlpha,
    /// `min(src alpha, 1 - dst alpha)` for color, 1 for alpha.
    SrcAlphaSaturated,
    /// The blend constant ([`CommandList::set_blend_constants`]).
    ///
    /// [`CommandList::set_blend_constants`]: crate::CommandList::set_blend_constants
    Constant,
    OneMinusConstant,
    /// The pixel shader's second output (`SV_Target1`) blended into target 0: dual-source
    /// blending. Only with a single color target.
    Src1Color,
    OneMinusSrc1Color,
    Src1Alpha,
    OneMinusSrc1Alpha,
}

impl BlendFactor {
    /// Reads the second pixel shader output (dual-source blending).
    pub const fn is_dual_source(self) -> bool {
        matches!(
            self,
            Self::Src1Color | Self::OneMinusSrc1Color | Self::Src1Alpha | Self::OneMinusSrc1Alpha
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BlendOp {
    Add,
    Subtract,
    ReverseSubtract,
    Min,
    Max,
}

/// `result = src * src_factor <op> dst * dst_factor`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BlendComponent {
    pub src_factor: BlendFactor,
    pub dst_factor: BlendFactor,
    pub op: BlendOp,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BlendState {
    pub color: BlendComponent,
    pub alpha: BlendComponent,
}

impl BlendState {
    /// Straight alpha: `src * a + dst * (1 - a)`.
    pub const ALPHA: Self = Self {
        color: BlendComponent {
            src_factor: BlendFactor::SrcAlpha,
            dst_factor: BlendFactor::OneMinusSrcAlpha,
            op: BlendOp::Add,
        },
        alpha: BlendComponent {
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::OneMinusSrcAlpha,
            op: BlendOp::Add,
        },
    };
    /// Premultiplied alpha: `src + dst * (1 - a)`.
    pub const PREMULTIPLIED: Self = Self {
        color: BlendComponent {
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::OneMinusSrcAlpha,
            op: BlendOp::Add,
        },
        alpha: BlendComponent {
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::OneMinusSrcAlpha,
            op: BlendOp::Add,
        },
    };
}

flags! {
    ColorWrites {
        R = 0x1,
        G = 0x2,
        B = 0x4,
        A = 0x8,
        ALL = 0xf,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ColorTargetState {
    pub format: Format,
    pub blend: Option<BlendState>,
    pub write_mask: ColorWrites,
}

impl ColorTargetState {
    /// No blending, all channels written.
    pub const fn new(format: Format) -> Self {
        Self {
            format,
            blend: None,
            write_mask: ColorWrites::ALL,
        }
    }
}

/// How a pipeline rasterizes into multisampled targets.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MultisampleState {
    /// Samples per pixel; equals the attachments' [`TextureDesc::sample_count`].
    pub count: u32,
    /// Samples a pixel may write (bit `n`: sample `n`).
    pub mask: u32,
    /// Derives coverage from the pixel shader's first alpha output (foliage, fences).
    pub alpha_to_coverage: bool,
}

impl MultisampleState {
    /// One sample: no multisampling.
    pub const NONE: Self = Self::samples(1);

    /// `count` samples, all writable, no alpha to coverage.
    pub const fn samples(count: u32) -> Self {
        Self {
            count,
            mask: u32::MAX,
            alpha_to_coverage: false,
        }
    }
}

impl Default for MultisampleState {
    fn default() -> Self {
        Self::NONE
    }
}

/// A graphics pipeline. There is no vertex input layout: vertex shaders fetch their vertices
/// from bindless buffers (vertex pulling). Index buffers stay fixed-function.
#[derive(Clone, Copy, Debug)]
pub struct GraphicsPipelineDesc<'a> {
    pub vertex: ShaderModule<'a>,
    /// `None`: depth-only.
    pub pixel: Option<ShaderModule<'a>>,
    pub topology: PrimitiveTopology,
    /// Strip topologies only: an index of all ones (`0xffff` / `0xffffffff` for this index
    /// format) restarts the strip. The pipeline then draws only with indices of that format.
    pub primitive_restart: Option<IndexFormat>,
    pub raster: RasterState,
    pub depth: Option<DepthState>,
    /// At most [`MAX_COLOR_ATTACHMENTS`]; formats must match the render pass's attachments.
    pub color_targets: &'a [ColorTargetState],
    pub multisample: MultisampleState,
    /// Where compiled pipelines are looked up and stored, to skip driver compilation next time.
    pub cache: Option<&'a crate::PipelineCache>,
}

#[derive(Clone, Copy, Debug)]
pub struct ComputePipelineDesc<'a> {
    pub shader: ShaderModule<'a>,
    /// As in [`GraphicsPipelineDesc::cache`].
    pub cache: Option<&'a crate::PipelineCache>,
}

// --- Commands ---

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IndexFormat {
    U16,
    U32,
}

/// In render target pixels, origin top-left; depth range usually 0..1.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub min_depth: f32,
    pub max_depth: f32,
}

impl Viewport {
    /// The whole `width × height` target, depth 0..1.
    pub const fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: width as f32,
            height: height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        }
    }
}

/// In pixels, origin top-left.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// A region copied between a buffer and one subresource of a texture.
///
/// The buffer holds rows of texel blocks `bytes_per_row` apart, a multiple of
/// [`TEXTURE_ROW_PITCH_ALIGNMENT`]; `buffer_offset` is a multiple of [`TEXTURE_OFFSET_ALIGNMENT`].
/// For block-compressed formats the region is in texels but block-aligned. Volume textures copy
/// `depth` slices from `z`, each `height / block` rows, one after another in the buffer.
/// Depth-stencil formats copy their depth plane ([`Format::copy_block_size`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BufferTextureCopy {
    pub buffer_offset: u64,
    pub bytes_per_row: u32,
    pub mip: u32,
    pub layer: u32,
    pub x: u32,
    pub y: u32,
    pub z: u32,
    pub width: u32,
    pub height: u32,
    /// Slices: 1 for 2D textures.
    pub depth: u32,
}

impl BufferTextureCopy {
    /// All of mip `mip`, layer `layer` of `desc`, tightly packed rows (aligned up), at `offset`.
    pub const fn whole_mip(desc: &TextureDesc, mip: u32, layer: u32, offset: u64) -> Self {
        let (width, height) = desc.mip_size(mip);
        Self {
            buffer_offset: offset,
            bytes_per_row: aligned_row_pitch(desc.format, width),
            mip,
            layer,
            x: 0,
            y: 0,
            z: 0,
            width,
            height,
            depth: desc.mip_depth(mip),
        }
    }
}

/// Bytes per row of a `width`-texel region of `format`, aligned up to
/// [`TEXTURE_ROW_PITCH_ALIGNMENT`].
pub const fn aligned_row_pitch(format: Format, width: u32) -> u32 {
    let blocks = width.div_ceil(format.block_extent());
    (blocks * format.copy_block_size()).next_multiple_of(TEXTURE_ROW_PITCH_ALIGNMENT)
}

flags! {
    /// Pipeline stages a barrier waits for (before) or blocks (after). Bit values match D3D12's
    /// `D3D12_BARRIER_SYNC`; Vulkan maps them to `VkPipelineStageFlags2`.
    Stage {
        NONE = 0,
        ALL = 0x1,
        DRAW = 0x2,
        INDEX_INPUT = 0x4,
        VERTEX_SHADING = 0x8,
        PIXEL_SHADING = 0x10,
        DEPTH_STENCIL = 0x20,
        RENDER_TARGET = 0x40,
        COMPUTE_SHADING = 0x80,
        RAYTRACING = 0x100,
        COPY = 0x200,
        RESOLVE = 0x400,
        /// Indirect argument reads and predication.
        EXECUTE_INDIRECT = 0x800,
        ALL_SHADING = 0x1000,
        NON_PIXEL_SHADING = 0x2000,
        /// [`CommandList::write_compacted_sizes`](crate::CommandList::write_compacted_sizes).
        EMIT_ACCELERATION_STRUCTURE_INFO = 0x4000,
        /// [`CommandList::fill_buffer`](crate::CommandList::fill_buffer) and
        /// [`CommandList::clear_storage_texture`](crate::CommandList::clear_storage_texture).
        CLEAR_STORAGE = 0x8000,
        BUILD_ACCELERATION_STRUCTURE = 0x80_0000,
        COPY_ACCELERATION_STRUCTURE = 0x100_0000,
    }
}

flags! {
    /// How a resource is accessed. Bit values match D3D12's `D3D12_BARRIER_ACCESS`, except that
    /// [`NONE`](Self::NONE) (Vulkan's 0) is D3D12's `NO_ACCESS`.
    Access {
        NONE = 0,
        VERTEX_BUFFER = 0x1,
        CONSTANT_BUFFER = 0x2,
        INDEX_BUFFER = 0x4,
        RENDER_TARGET = 0x8,
        UNORDERED_ACCESS = 0x10,
        DEPTH_STENCIL_WRITE = 0x20,
        DEPTH_STENCIL_READ = 0x40,
        SHADER_RESOURCE = 0x80,
        /// Indirect arguments, counts and predicates.
        INDIRECT_ARGUMENT = 0x200,
        COPY_DST = 0x400,
        COPY_SRC = 0x800,
        RESOLVE_DST = 0x1000,
        RESOLVE_SRC = 0x2000,
        /// Ray queries traversing it, and builds or copies reading it.
        ACCELERATION_STRUCTURE_READ = 0x4000,
        /// Builds and copies writing it.
        ACCELERATION_STRUCTURE_WRITE = 0x8000,
        SHADING_RATE_SOURCE = 0x1_0000,
    }
}

/// How a texture's memory is arranged. Maps 1:1 to D3D12 barrier layouts and Vulkan image
/// layouts; legacy D3D12 derives a resource state from it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Layout {
    /// Contents may be discarded. Only valid as a barrier's `before`; the barrier's
    /// [`undefined_from`](TextureBarrier::undefined_from) names the layout the texture is really
    /// in, for legacy D3D12.
    Undefined,
    /// Swapchain image ready for presentation (D3D12: the common layout).
    Present,
    /// Readable by any stage and copies; the layout of a new texture.
    Common,
    RenderTarget,
    UnorderedAccess,
    DepthStencilWrite,
    DepthStencilRead,
    ShaderResource,
    CopySrc,
    CopyDst,
    ResolveSrc,
    ResolveDst,
    /// Read as a shading-rate image.
    ShadingRateSource,
}

/// A texture's synchronization state on one side of a barrier.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TextureState {
    pub stage: Stage,
    pub access: Access,
    pub layout: Layout,
}

impl TextureState {
    /// No prior work to wait for, contents discarded.
    pub const UNDEFINED: Self = Self {
        stage: Stage::NONE,
        access: Access::NONE,
        layout: Layout::Undefined,
    };
    /// Handed to (or back from) the presentation engine.
    pub const PRESENT: Self = Self {
        stage: Stage::NONE,
        access: Access::NONE,
        layout: Layout::Present,
    };
    pub const RENDER_TARGET: Self = Self {
        stage: Stage::RENDER_TARGET,
        access: Access::RENDER_TARGET,
        layout: Layout::RenderTarget,
    };
    pub const PIXEL_SHADER_READ: Self = Self {
        stage: Stage::PIXEL_SHADING,
        access: Access::SHADER_RESOURCE,
        layout: Layout::ShaderResource,
    };
    pub const COPY_SRC: Self = Self {
        stage: Stage::COPY,
        access: Access::COPY_SRC,
        layout: Layout::CopySrc,
    };
    pub const COPY_DST: Self = Self {
        stage: Stage::COPY,
        access: Access::COPY_DST,
        layout: Layout::CopyDst,
    };
    pub const DEPTH_WRITE: Self = Self {
        stage: Stage::DEPTH_STENCIL,
        access: Access::DEPTH_STENCIL_WRITE,
        layout: Layout::DepthStencilWrite,
    };
    pub const DEPTH_READ: Self = Self {
        stage: Stage::DEPTH_STENCIL,
        access: Access::DEPTH_STENCIL_READ,
        layout: Layout::DepthStencilRead,
    };
    /// Read by any shader stage.
    pub const SHADER_READ: Self = Self {
        stage: Stage::ALL_SHADING,
        access: Access::SHADER_RESOURCE,
        layout: Layout::ShaderResource,
    };
    /// Read and written by compute shaders.
    pub const COMPUTE_WRITE: Self = Self {
        stage: Stage::COMPUTE_SHADING,
        access: Access::UNORDERED_ACCESS,
        layout: Layout::UnorderedAccess,
    };
    /// Cleared by [`CommandList::clear_storage_texture`](crate::CommandList::clear_storage_texture).
    pub const CLEAR_STORAGE: Self = Self {
        stage: Stage::CLEAR_STORAGE,
        access: Access::UNORDERED_ACCESS,
        layout: Layout::UnorderedAccess,
    };
    /// The source of [`CommandList::resolve_texture`](crate::CommandList::resolve_texture).
    pub const RESOLVE_SRC: Self = Self {
        stage: Stage::RESOLVE,
        access: Access::RESOLVE_SRC,
        layout: Layout::ResolveSrc,
    };
    pub const RESOLVE_DST: Self = Self {
        stage: Stage::RESOLVE,
        access: Access::RESOLVE_DST,
        layout: Layout::ResolveDst,
    };
    /// Read as the shading-rate image of draws.
    pub const SHADING_RATE: Self = Self {
        stage: Stage::PIXEL_SHADING,
        access: Access::SHADING_RATE_SOURCE,
        layout: Layout::ShadingRateSource,
    };
}

/// A buffer's synchronization state on one side of a barrier. Buffers have no layout.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BufferState {
    pub stage: Stage,
    pub access: Access,
}

impl BufferState {
    /// Nothing to wait for: a new buffer, or one only the CPU has touched.
    pub const NONE: Self = Self {
        stage: Stage::NONE,
        access: Access::NONE,
    };
    pub const COPY_SRC: Self = Self {
        stage: Stage::COPY,
        access: Access::COPY_SRC,
    };
    pub const COPY_DST: Self = Self {
        stage: Stage::COPY,
        access: Access::COPY_DST,
    };
    pub const INDEX: Self = Self {
        stage: Stage::INDEX_INPUT,
        access: Access::INDEX_BUFFER,
    };
    /// Read by any shader stage through uniform, structured or raw views.
    pub const SHADER_READ: Self = Self {
        stage: Stage::ALL_SHADING,
        access: Access::SHADER_RESOURCE.union(Access::CONSTANT_BUFFER),
    };
    pub const COMPUTE_WRITE: Self = Self {
        stage: Stage::COMPUTE_SHADING,
        access: Access::UNORDERED_ACCESS,
    };
    /// Arguments and counts of indirect commands, and predicates.
    pub const INDIRECT: Self = Self {
        stage: Stage::EXECUTE_INDIRECT,
        access: Access::INDIRECT_ARGUMENT,
    };
    /// Filled by [`CommandList::fill_buffer`](crate::CommandList::fill_buffer).
    pub const CLEAR_STORAGE: Self = Self {
        stage: Stage::CLEAR_STORAGE,
        access: Access::UNORDERED_ACCESS,
    };
    /// Vertices, indices, transforms, AABBs or instances read by acceleration structure builds.
    pub const BUILD_INPUT: Self = Self {
        stage: Stage::BUILD_ACCELERATION_STRUCTURE,
        access: Access::SHADER_RESOURCE,
    };
    /// Scratch memory of acceleration structure builds.
    pub const BUILD_SCRATCH: Self = Self {
        stage: Stage::BUILD_ACCELERATION_STRUCTURE,
        access: Access::UNORDERED_ACCESS,
    };
    /// An [`AccelerationStructure::buffer`](crate::AccelerationStructure::buffer) being built,
    /// updated or copied into.
    pub const ACCELERATION_STRUCTURE_WRITE: Self = Self {
        stage: Stage::BUILD_ACCELERATION_STRUCTURE.union(Stage::COPY_ACCELERATION_STRUCTURE),
        access: Access::ACCELERATION_STRUCTURE_WRITE,
    };
    /// An acceleration structure traversed by ray queries in any shader, or read by builds and
    /// copies.
    pub const ACCELERATION_STRUCTURE_READ: Self = Self {
        stage: Stage::ALL_SHADING
            .union(Stage::BUILD_ACCELERATION_STRUCTURE)
            .union(Stage::COPY_ACCELERATION_STRUCTURE)
            .union(Stage::EMIT_ACCELERATION_STRUCTURE_INFO),
        access: Access::ACCELERATION_STRUCTURE_READ,
    };
}

/// An execution and memory dependency on a whole buffer.
///
/// On D3D12 without enhanced barriers every buffer returns to the common state between
/// submissions, so a barrier must be recorded in the same submission as its `before` access (the
/// usual case: a copy or dispatch followed by its consumers). Across submissions none is needed.
#[derive(Clone, Copy, Debug)]
pub struct BufferBarrier<'a> {
    pub buffer: &'a Buffer,
    pub before: BufferState,
    pub after: BufferState,
}

/// A layout transition and/or execution and memory dependency on a texture: the work and accesses
/// of `before` complete and become visible before those of `after` start.
#[derive(Clone, Copy, Debug)]
pub struct TextureBarrier<'a> {
    pub texture: &'a Texture,
    pub before: TextureState,
    pub after: TextureState,
    pub range: SubresourceRange,
    /// When `before.layout` is [`Layout::Undefined`]: the layout the range is actually in (the
    /// `after` of its last barrier in execution order; [`Layout::Common`] for a new texture or
    /// one taking over aliased memory). D3D12 without enhanced barriers must name the real state
    /// even when the contents are discarded; every other path ignores it. The render graph knows
    /// it; recording order does not (lists recorded in parallel run in another order).
    pub undefined_from: Layout,
}

impl<'a> TextureBarrier<'a> {
    /// A barrier over every subresource. A `before` of [`Layout::Undefined`] is taken to come
    /// from [`Layout::Common`]; set [`undefined_from`](Self::undefined_from) otherwise.
    pub const fn new(texture: &'a Texture, before: TextureState, after: TextureState) -> Self {
        Self {
            texture,
            before,
            after,
            range: SubresourceRange::ALL,
            undefined_from: Layout::Common,
        }
    }

    /// Discards the contents of a texture last used in state `previous`: waits for that work
    /// (`previous.stage`) without keeping what it wrote, and names its layout for legacy D3D12.
    /// The way to reuse a texture whose old contents no longer matter (a transient target from
    /// last frame, an aliased resource).
    pub const fn discard(
        texture: &'a Texture,
        previous: TextureState,
        after: TextureState,
    ) -> Self {
        Self {
            undefined_from: previous.layout,
            ..Self::new(
                texture,
                TextureState {
                    stage: previous.stage,
                    access: Access::NONE,
                    layout: Layout::Undefined,
                },
                after,
            )
        }
    }
}

// --- Render passes ---

/// What a render pass does with an attachment's existing contents. `T` is the clear value: RGBA
/// for color, depth for depth.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LoadOp<T = [f32; 4]> {
    Load,
    /// Clears to this value (colors are linear; an sRGB target encodes them).
    Clear(T),
    /// Contents are undefined; cheapest on tile-based GPUs.
    DontCare,
}

/// What a render pass does with an attachment's contents at its end.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StoreOp {
    Store,
    /// Contents may be discarded (transient attachments, depth after the last pass).
    DontCare,
}

#[derive(Clone, Copy, Debug)]
pub struct ColorAttachment<'a> {
    pub view: &'a RenderTargetView,
    pub load: LoadOp,
    pub store: StoreOp,
}

/// Most color attachments a render pass may have (D3D12 and Vulkan's guaranteed minimum).
pub const MAX_COLOR_ATTACHMENTS: usize = 8;

#[derive(Clone, Copy, Debug)]
pub struct DepthAttachment<'a> {
    pub view: &'a DepthStencilView,
    /// Reverse-Z clears to 0 (the far plane).
    pub load: LoadOp<f32>,
    pub store: StoreOp,
    /// The stencil plane's; ignored for formats without stencil.
    pub stencil_load: LoadOp<u8>,
    pub stencil_store: StoreOp,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RenderPassDesc<'a> {
    /// At most [`MAX_COLOR_ATTACHMENTS`]. Each must already be in [`Layout::RenderTarget`].
    pub color: &'a [ColorAttachment<'a>],
    /// Must be in [`Layout::DepthStencilWrite`] (or `DepthStencilRead` for a read-only view).
    pub depth: Option<DepthAttachment<'a>>,
}

// --- Presentation ---

/// The native window a swapchain presents to.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SurfaceHandle {
    Win32 {
        hwnd: NonNull<c_void>,
        hinstance: NonNull<c_void>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum PresentMode {
    /// Wait for vertical blank; never tears. Always supported.
    #[default]
    Fifo,
    /// Do not wait; the compositor shows the newest finished frame. No tearing.
    Mailbox,
    /// Do not wait, tear if the display allows it ([`Features::tearing`]); otherwise like
    /// [`Mailbox`](Self::Mailbox).
    Immediate,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SwapchainDesc {
    /// Physical pixels; usually the window's client size.
    pub width: u32,
    pub height: u32,
    /// sRGB formats get linear images with sRGB render target views where the API requires it.
    pub format: Format,
    /// Images in the chain (2..=16). 3 lets the GPU start a frame while one image is on screen
    /// and the next is queued.
    pub image_count: u32,
    pub present_mode: PresentMode,
    /// Frames the CPU may queue ahead of the display before
    /// [`acquire`](crate::Swapchain::acquire) blocks: the latency/throughput trade-off.
    pub max_frame_latency: u32,
    /// How the display interprets the images. HDR needs a matching format:
    /// [`ColorSpace::Hdr10`] takes [`Format::Rgb10a2Unorm`], [`ColorSpace::ScRgb`]
    /// [`Format::Rgba16Float`]. Change it later with
    /// [`Swapchain::set_color_space`](crate::Swapchain::set_color_space).
    pub color_space: ColorSpace,
}

impl Default for SwapchainDesc {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            format: Format::Bgra8UnormSrgb,
            image_count: 3,
            present_mode: PresentMode::Fifo,
            max_frame_latency: 2,
            color_space: ColorSpace::Srgb,
        }
    }
}

/// How a display interprets swapchain values.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum ColorSpace {
    /// Rec. 709 primaries, sRGB transfer: SDR.
    #[default]
    Srgb,
    /// Rec. 2020 primaries, SMPTE ST 2084 (PQ) transfer, 10-bit: the HDR10 signal. Values are
    /// PQ-encoded absolute luminance (10 000 nits at 1.0).
    Hdr10,
    /// Rec. 709 primaries, linear, 16-bit float: 1.0 is 80 nits and values may exceed 1 or go
    /// negative for wider gamuts. The compositor converts to what the display takes.
    ScRgb,
}

/// The mastering metadata of HDR10 content, which the display uses to tone-map it
/// ([`Swapchain::set_hdr_metadata`](crate::Swapchain::set_hdr_metadata)). Chromaticities are CIE
/// 1931 xy; luminances are in nits.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct HdrMetadata {
    pub red_primary: [f32; 2],
    pub green_primary: [f32; 2],
    pub blue_primary: [f32; 2],
    pub white_point: [f32; 2],
    pub max_mastering_luminance: f32,
    pub min_mastering_luminance: f32,
    /// MaxCLL: the brightest pixel of the content.
    pub max_content_light_level: f32,
    /// MaxFALL: the brightest frame average.
    pub max_frame_average_light_level: f32,
}

impl HdrMetadata {
    /// Rec. 2020 primaries and a D65 white point, with the given luminance range.
    pub const fn rec2020(min_luminance: f32, max_luminance: f32) -> Self {
        Self {
            red_primary: [0.708, 0.292],
            green_primary: [0.170, 0.797],
            blue_primary: [0.131, 0.046],
            white_point: [0.3127, 0.3290],
            max_mastering_luminance: max_luminance,
            min_mastering_luminance: min_luminance,
            max_content_light_level: max_luminance,
            max_frame_average_light_level: max_luminance,
        }
    }
}

const OUTPUT_NAME_CAPACITY: usize = 64;

/// A display attached to an adapter ([`Instance::outputs`](crate::Instance::outputs),
/// [`Swapchain::output`](crate::Swapchain::output)): where it is and what it can show.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct OutputInfo {
    /// Its rectangle on the desktop, in physical pixels.
    pub desktop: Rect,
    /// Bits per color channel the display is driven at.
    pub bits_per_color: u32,
    /// The color space the OS drives it in: [`ColorSpace::Hdr10`] when HDR is on.
    pub color_space: ColorSpace,
    pub red_primary: [f32; 2],
    pub green_primary: [f32; 2],
    pub blue_primary: [f32; 2],
    pub white_point: [f32; 2],
    /// Nits.
    pub min_luminance: f32,
    pub max_luminance: f32,
    pub max_full_frame_luminance: f32,
    name: [u8; OUTPUT_NAME_CAPACITY],
    name_len: u8,
}

impl OutputInfo {
    #[cfg_attr(
        any(marlin_platform = "linux", marlin_platform = "macos"),
        expect(dead_code, reason = "the stub backend has no outputs")
    )]
    pub(crate) fn new(name: &str, desktop: Rect) -> Self {
        let mut info = Self {
            desktop,
            bits_per_color: 8,
            color_space: ColorSpace::Srgb,
            red_primary: [0.0; 2],
            green_primary: [0.0; 2],
            blue_primary: [0.0; 2],
            white_point: [0.0; 2],
            min_luminance: 0.0,
            max_luminance: 0.0,
            max_full_frame_luminance: 0.0,
            name: [0; OUTPUT_NAME_CAPACITY],
            name_len: 0,
        };
        let mut len = 0;
        for c in name.chars() {
            if len + c.len_utf8() > OUTPUT_NAME_CAPACITY {
                break;
            }
            len += c.encode_utf8(&mut info.name[len..]).len();
        }
        info.name_len = len as u8;
        info
    }

    /// The OS's name for the display (`\\.\DISPLAY1` on Windows).
    pub fn name(&self) -> &str {
        // SAFETY: `new` encodes whole chars only.
        unsafe { core::str::from_utf8_unchecked(&self.name[..self.name_len as usize]) }
    }

    /// The OS presents it as HDR.
    pub fn is_hdr(&self) -> bool {
        self.color_space == ColorSpace::Hdr10
    }
}

/// Presentation timing ([`Swapchain::frame_statistics`](crate::Swapchain::frame_statistics)):
/// when the last vertical blank happened and which present it showed, for frame pacing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FrameStatistics {
    /// Presents that have reached the display so far.
    pub present_count: u32,
    /// The vertical blank count at which the last of them was shown.
    pub present_refresh_count: u32,
    /// The vertical blank count when the statistics were sampled.
    pub sync_refresh_count: u32,
    /// The CPU time of that vertical blank, in the OS's performance-counter ticks (Windows
    /// `QueryPerformanceCounter`).
    pub sync_cpu_time: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PresentStatus {
    Presented,
    /// The window is fully hidden (minimized, covered); presenting more often wastes power.
    Occluded,
}

// --- Queues and submission ---

/// One submission to a queue: GPU-side waits, then the lists in order, then the signals.
#[derive(Clone, Copy, Debug, Default)]
pub struct SubmitDesc<'a> {
    /// The queue does not start the lists before each fence reaches its value (another queue's
    /// work, or the CPU's signal). The CPU does not block.
    pub waits: &'a [(&'a crate::Fence, u64)],
    /// Lists recorded for this queue's type.
    pub lists: &'a [&'a crate::CommandList],
    /// Set once the lists have finished.
    pub signals: &'a [(&'a crate::Fence, u64)],
}

// --- Indirect commands ---

/// The push-constant word every indirect command writes its `constant` into, so the shaders a
/// multi-draw runs can tell its draws apart (D3D12 sets it from the argument buffer; Vulkan
/// backends will map it to the draw index).
pub const INDIRECT_CONSTANT_WORD: u32 = MAX_PUSH_CONSTANTS - 1;

/// Arguments of one indirect draw, as the argument buffer holds them (20 bytes).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct DrawIndirectArgs {
    /// Lands in push-constant word [`INDIRECT_CONSTANT_WORD`].
    pub constant: u32,
    pub vertex_count: u32,
    pub instance_count: u32,
    pub first_vertex: u32,
    pub first_instance: u32,
}

/// Arguments of one indirect indexed draw (24 bytes).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct DrawIndexedIndirectArgs {
    pub constant: u32,
    pub index_count: u32,
    pub instance_count: u32,
    pub first_index: u32,
    pub vertex_offset: i32,
    pub first_instance: u32,
}

/// Arguments of one indirect compute or mesh dispatch (16 bytes).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct DispatchIndirectArgs {
    pub constant: u32,
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

/// Where an indirect command finds its arguments: `max_count` records of the command's argument
/// type ([`DrawIndirectArgs`], [`DrawIndexedIndirectArgs`] or [`DispatchIndirectArgs`]), packed
/// from `offset`. With a count buffer, the first `count` of them run (the `u32` there, clamped
/// to `max_count`), else all of them.
#[derive(Clone, Copy, Debug)]
pub struct IndirectArgs<'a> {
    /// [`BufferUsage::INDIRECT`].
    pub buffer: &'a crate::Buffer,
    /// A multiple of 4.
    pub offset: u64,
    pub max_count: u32,
    pub count: Option<(&'a crate::Buffer, u64)>,
}

// --- Mesh pipelines ---

/// A mesh shader pipeline: an optional amplification (task) stage, the mesh stage, and the
/// fixed-function state of a graphics pipeline.
#[derive(Clone, Copy, Debug)]
pub struct MeshPipelineDesc<'a> {
    pub amplification: Option<ShaderModule<'a>>,
    pub mesh: ShaderModule<'a>,
    /// `None`: depth-only.
    pub pixel: Option<ShaderModule<'a>>,
    pub raster: RasterState,
    pub depth: Option<DepthState>,
    pub color_targets: &'a [ColorTargetState],
    pub multisample: MultisampleState,
    /// As in [`GraphicsPipelineDesc::cache`].
    pub cache: Option<&'a crate::PipelineCache>,
}

// --- Queries ---

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum QueryKind {
    /// GPU clock values, [`Queue::timestamp_frequency`](crate::Queue::timestamp_frequency) ticks
    /// per second, written by [`CommandList::write_timestamp`](crate::CommandList::write_timestamp)
    /// on graphics and compute lists.
    Timestamp,
    /// Timestamps written on copy lists (D3D12 keeps them in their own heap type). Requires
    /// [`Features::copy_queue_timestamps`].
    CopyTimestamp,
    /// The number of samples that passed the depth and stencil tests between
    /// [`begin_query`](crate::CommandList::begin_query) and
    /// [`end_query`](crate::CommandList::end_query), on graphics lists.
    Occlusion,
    /// As [`Occlusion`](Self::Occlusion), but only 0 or 1 (may be cheaper): for predication.
    BinaryOcclusion,
    /// [`PipelineStatistics`] counted between `begin_query` and `end_query` on graphics and
    /// compute lists. Requires [`Features::pipeline_statistics`]. For profiling and debugging:
    /// NVIDIA drivers (seen on 610.88) return garbage for a graphics query at the very start of
    /// a command list, before other GPU work in it.
    PipelineStatistics,
}

impl QueryKind {
    /// Bytes [`resolve_queries`](crate::CommandList::resolve_queries) writes per query: a `u64`,
    /// or one [`PipelineStatistics`].
    pub const fn result_size(self) -> u64 {
        match self {
            Self::PipelineStatistics => size_of::<PipelineStatistics>() as u64,
            _ => 8,
        }
    }

    /// Measured over a begin/end pair rather than written at a point.
    pub const fn is_scoped(self) -> bool {
        matches!(
            self,
            Self::Occlusion | Self::BinaryOcclusion | Self::PipelineStatistics
        )
    }
}

/// One [`QueryKind::PipelineStatistics`] result as resolved into a buffer (112 bytes, D3D12's
/// `D3D12_QUERY_DATA_PIPELINE_STATISTICS1` order).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct PipelineStatistics {
    /// Vertices and primitives the input assembler read.
    pub input_vertices: u64,
    pub input_primitives: u64,
    pub vertex_invocations: u64,
    /// Always 0: there are no geometry shaders.
    pub geometry_invocations: u64,
    pub geometry_primitives: u64,
    /// Primitives sent to and emitted by clipping.
    pub clipper_invocations: u64,
    pub clipper_primitives: u64,
    pub pixel_invocations: u64,
    /// Always 0: there is no tessellation.
    pub hull_invocations: u64,
    pub domain_invocations: u64,
    pub compute_invocations: u64,
    pub amplification_invocations: u64,
    pub mesh_invocations: u64,
    pub mesh_primitives: u64,
}

/// When [`CommandList::set_predication`](crate::CommandList::set_predication) skips work: on
/// the 64-bit value at the predicate's offset.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PredicateSkip {
    /// Skip while it is 0 (an occlusion query saw nothing).
    IfZero,
    IfNonZero,
}

/// GPU and CPU clocks sampled at the same moment ([`Queue::clock_calibration`]), to place GPU
/// timestamps on the CPU timeline.
///
/// [`Queue::clock_calibration`]: crate::Queue::clock_calibration
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ClockCalibration {
    /// In [`Queue::timestamp_frequency`](crate::Queue::timestamp_frequency) ticks.
    pub gpu: u64,
    /// In the OS's performance-counter ticks (Windows `QueryPerformanceCounter`).
    pub cpu: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct QueryPoolDesc {
    pub kind: QueryKind,
    pub count: u32,
}

// --- Heaps ---

/// What a heap may hold where heaps are not mixed ([`Features::mixed_heaps`] false).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum HeapKind {
    /// Anything (requires [`Features::mixed_heaps`]).
    #[default]
    Any,
    Buffers,
    /// Textures that are neither render targets nor depth.
    Textures,
    /// Render target and depth textures.
    Targets,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct HeapDesc {
    pub size: u64,
    pub memory: MemoryLocation,
    pub kind: HeapKind,
}

/// The size and alignment a resource takes in a heap.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MemoryRequirements {
    pub size: u64,
    pub alignment: u64,
}

/// Alignment of placed resources' offsets that is always enough for single-sampled resources
/// (64 KiB; D3D12's default). Multisampled textures may need [`MSAA_PLACEMENT_ALIGNMENT`].
pub const PLACEMENT_ALIGNMENT: u64 = 64 << 10;
/// Alignment of placed multisampled textures that is always enough (4 MiB).
pub const MSAA_PLACEMENT_ALIGNMENT: u64 = 4 << 20;

// --- Texture copies ---

/// A box copied between two textures of compatible formats: `size` texels (block-aligned for
/// compressed formats) from `src_offset` in subresource (`src_mip`, `src_layer`) to `dst_offset`
/// in (`dst_mip`, `dst_layer`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TextureCopy {
    pub src_mip: u32,
    pub src_layer: u32,
    pub src_offset: [u32; 3],
    pub dst_mip: u32,
    pub dst_layer: u32,
    pub dst_offset: [u32; 3],
    /// Width, height and depth.
    pub size: [u32; 3],
}

/// One multisample resolve ([`CommandList::resolve_texture`](crate::CommandList::resolve_texture)):
/// subresource (`src_mip`, `src_layer`) of a multisampled texture averaged into (`dst_mip`,
/// `dst_layer`) of a single-sampled one of the same size.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct TextureResolve {
    pub src_mip: u32,
    pub src_layer: u32,
    pub dst_mip: u32,
    pub dst_layer: u32,
    /// `None`: the destination's format. Otherwise its sRGB/linear twin, which decides whether
    /// the average is taken in linear space.
    pub format: Option<Format>,
}

// --- Clears and markers ---

/// The value [`CommandList::clear_storage_texture`](crate::CommandList::clear_storage_texture)
/// writes, in the representation the texture's format reads: floats for float and normalized
/// formats, integers for integer formats.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ClearColor {
    Float([f32; 4]),
    Uint([u32; 4]),
    Sint([i32; 4]),
}

/// When [`CommandList::write_marker`](crate::CommandList::write_marker) writes its value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MarkerPoint {
    /// When the GPU reaches the marker, before earlier work has finished: "started".
    Reached,
    /// Once all earlier work in the list has finished: "done".
    Completed,
}

/// Whether [`Device::wait_for_fences`](crate::Device::wait_for_fences) waits for every fence or
/// the first.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FenceWait {
    All,
    Any,
}

// --- Memory budget and residency ---

/// One memory segment's numbers, in bytes ([`Device::memory_budget`]).
///
/// [`Device::memory_budget`]: crate::Device::memory_budget
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct MemoryUsage {
    /// What the OS lets this process use without paging. It moves as other applications come
    /// and go; staying under it is the application's job.
    pub budget: u64,
    /// What the process uses now.
    pub usage: u64,
    /// The most [`Device::set_memory_reservation`](crate::Device::set_memory_reservation) may
    /// reserve.
    pub available_for_reservation: u64,
    pub reservation: u64,
}

impl MemoryUsage {
    /// Bytes left before the budget is reached (0 when over it).
    pub const fn headroom(&self) -> u64 {
        self.budget.saturating_sub(self.usage)
    }
}

/// [`Device::memory_budget`](crate::Device::memory_budget): device-local memory (VRAM, or the
/// whole of memory on unified-memory adapters) and the system memory the GPU reaches over the bus.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct MemoryBudget {
    pub local: MemoryUsage,
    pub non_local: MemoryUsage,
}

/// Which memory the OS pages out first when it must ([`Device::set_residency_priority`]).
///
/// [`Device::set_residency_priority`]: crate::Device::set_residency_priority
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum ResidencyPriority {
    Minimum,
    Low,
    #[default]
    Normal,
    High,
    /// Render targets and anything a frame cannot do without.
    Maximum,
}

/// Something whose memory can be made resident or evicted as a whole.
#[derive(Clone, Copy, Debug)]
pub enum Pageable<'a> {
    Heap(&'a crate::Heap),
    /// A committed buffer (placed buffers page with their heap).
    Buffer(&'a crate::Buffer),
    /// A committed texture.
    Texture(&'a crate::Texture),
    AccelerationStructure(&'a crate::AccelerationStructure),
}

// --- Acceleration structures (ray queries) ---

/// Byte alignment of acceleration structure storage and build scratch memory.
pub const ACCELERATION_STRUCTURE_ALIGNMENT: u64 = 256;
/// Byte alignment of the instance records of a top-level build.
pub const INSTANCE_ALIGNMENT: u64 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AccelerationStructureKind {
    /// Geometry: triangles or AABBs.
    BottomLevel,
    /// Instances of bottom-level structures; what ray queries traverse.
    TopLevel,
}

flags! {
    /// How an acceleration structure is built.
    BuildFlags {
        /// Later builds may update it in place ([`BuildAccelerationStructure::update`]).
        ALLOW_UPDATE = 0x1,
        /// [`CommandList::write_compacted_sizes`](crate::CommandList::write_compacted_sizes) and
        /// compacting copies may follow.
        ALLOW_COMPACTION = 0x2,
        PREFER_FAST_TRACE = 0x4,
        PREFER_FAST_BUILD = 0x8,
        MINIMIZE_MEMORY = 0x10,
    }
}

flags! {
    GeometryFlags {
        /// No any-hit processing: ray queries see hits as committed candidates directly.
        OPAQUE = 0x1,
        /// Candidates are reported once per primitive.
        NO_DUPLICATE_ANY_HIT = 0x2,
    }
}

flags! {
    /// Per-instance overrides in [`AccelerationStructureInstance`].
    InstanceFlags {
        TRIANGLE_CULL_DISABLE = 0x1,
        /// Counter-clockwise triangles (in object space, seen along the ray) are front faces.
        TRIANGLE_FRONT_COUNTERCLOCKWISE = 0x2,
        FORCE_OPAQUE = 0x4,
        FORCE_NON_OPAQUE = 0x8,
    }
}

/// Vertex position formats bottom-level builds read.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum VertexFormat {
    Float3,
    /// Z is 0.
    Float2,
    /// Three halfs, the fourth ignored.
    Half4,
    Half2,
    Snorm16x4,
    Snorm16x2,
}

impl VertexFormat {
    /// The alignment the vertex offset and stride need (one component).
    pub const fn component_size(self) -> u64 {
        match self {
            Self::Float3 | Self::Float2 => 4,
            _ => 2,
        }
    }

    /// Bytes of one vertex position.
    pub const fn size(self) -> u64 {
        match self {
            Self::Float3 => 12,
            Self::Float2 | Self::Half4 | Self::Snorm16x4 => 8,
            Self::Half2 | Self::Snorm16x2 => 4,
        }
    }
}

/// Triangles of a bottom-level structure. Buffers need
/// [`BufferUsage::ACCELERATION_STRUCTURE_INPUT`] and must be in [`BufferState::BUILD_INPUT`].
#[derive(Clone, Copy, Debug)]
pub struct AccelerationStructureTriangles<'a> {
    pub vertices: &'a crate::Buffer,
    pub vertex_offset: u64,
    pub vertex_stride: u64,
    pub vertex_format: VertexFormat,
    pub vertex_count: u32,
    /// Indexed triangles: the buffer, an offset aligned to the index size, and the format.
    pub indices: Option<(&'a crate::Buffer, u64, IndexFormat)>,
    /// Triangles: a third of the index count when indexed, else of `vertex_count`.
    pub triangle_count: u32,
    /// A row-major 3×4 `f32` matrix applied to the vertices, at a 16-byte aligned offset.
    pub transform: Option<(&'a crate::Buffer, u64)>,
    pub flags: GeometryFlags,
}

/// Axis-aligned boxes of a bottom-level structure (procedural geometry): `count` records of
/// six `f32` (min xyz, max xyz), `stride` (a multiple of 8) apart from an 8-byte aligned `offset`.
#[derive(Clone, Copy, Debug)]
pub struct AccelerationStructureAabbs<'a> {
    pub buffer: &'a crate::Buffer,
    pub offset: u64,
    pub stride: u64,
    pub count: u32,
    pub flags: GeometryFlags,
}

#[derive(Clone, Copy, Debug)]
pub enum AccelerationStructureGeometry<'a> {
    Triangles(AccelerationStructureTriangles<'a>),
    Aabbs(AccelerationStructureAabbs<'a>),
}

/// What an acceleration structure is built from. Sizes
/// ([`Device::acceleration_structure_sizes`]) depend only on the counts and formats, not on the
/// buffers' contents.
///
/// [`Device::acceleration_structure_sizes`]: crate::Device::acceleration_structure_sizes
#[derive(Clone, Copy, Debug)]
pub enum AccelerationStructureInputs<'a> {
    /// All triangles or all AABBs.
    BottomLevel(&'a [AccelerationStructureGeometry<'a>]),
    /// `count` [`AccelerationStructureInstance`] records at `offset` ([`INSTANCE_ALIGNMENT`]) in
    /// `instances` ([`BufferUsage::ACCELERATION_STRUCTURE_INPUT`]).
    TopLevel {
        instances: &'a crate::Buffer,
        offset: u64,
        count: u32,
    },
}

impl AccelerationStructureInputs<'_> {
    pub const fn kind(&self) -> AccelerationStructureKind {
        match self {
            Self::BottomLevel(_) => AccelerationStructureKind::BottomLevel,
            Self::TopLevel { .. } => AccelerationStructureKind::TopLevel,
        }
    }
}

/// Memory an acceleration structure and its builds need, in bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct AccelerationStructureSizes {
    /// For [`Device::create_acceleration_structure`](crate::Device::create_acceleration_structure).
    pub size: u64,
    pub build_scratch_size: u64,
    pub update_scratch_size: u64,
}

/// One instance record of a top-level build (64 bytes, the layout D3D12 and Vulkan share).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct AccelerationStructureInstance {
    /// Object-to-world, row-major 3×4.
    pub transform: [[f32; 4]; 3],
    /// `InstanceID()` in the low 24 bits, the visibility mask in the high 8.
    id_and_mask: u32,
    /// A 24-bit value shaders read with `CommittedInstanceContributionToHitGroupIndex()`, and the
    /// [`InstanceFlags`] in the high 8 bits.
    contribution_and_flags: u32,
    /// [`AccelerationStructure::instance_address`](crate::AccelerationStructure::instance_address).
    acceleration_structure: u64,
}

impl AccelerationStructureInstance {
    /// The identity transform.
    pub const IDENTITY: [[f32; 4]; 3] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ];

    /// `id` and `contribution` keep their low 24 bits.
    pub fn new(
        transform: [[f32; 4]; 3],
        id: u32,
        mask: u8,
        contribution: u32,
        flags: InstanceFlags,
        blas: &crate::AccelerationStructure,
    ) -> Self {
        Self {
            transform,
            id_and_mask: (id & 0xff_ffff) | (mask as u32) << 24,
            contribution_and_flags: (contribution & 0xff_ffff) | (flags.bits() & 0xff) << 24,
            acceleration_structure: blas.instance_address(),
        }
    }

    pub const fn id(&self) -> u32 {
        self.id_and_mask & 0xff_ffff
    }

    pub const fn mask(&self) -> u8 {
        (self.id_and_mask >> 24) as u8
    }

    /// The record's bytes, to write into an instance buffer.
    pub fn as_bytes(&self) -> &[u8; 64] {
        // SAFETY: repr(C), 64 bytes of plain data without padding.
        unsafe { &*(self as *const Self).cast() }
    }
}

const _: () = assert!(size_of::<AccelerationStructureInstance>() == 64);

/// One acceleration structure build ([`CommandList::build_acceleration_structure`]).
///
/// [`CommandList::build_acceleration_structure`]: crate::CommandList::build_acceleration_structure
#[derive(Clone, Copy, Debug)]
pub struct BuildAccelerationStructure<'a> {
    pub dst: &'a crate::AccelerationStructure,
    pub inputs: AccelerationStructureInputs<'a>,
    pub flags: BuildFlags,
    /// Refit from this earlier build (built with [`BuildFlags::ALLOW_UPDATE`], same inputs
    /// except vertex positions and instance transforms); may be `dst` itself.
    pub update: Option<&'a crate::AccelerationStructure>,
    /// [`BufferUsage::SHADER_WRITE`] memory of at least the build (or update) scratch size, at
    /// an [`ACCELERATION_STRUCTURE_ALIGNMENT`]-aligned offset.
    pub scratch: &'a crate::Buffer,
    pub scratch_offset: u64,
}

/// How [`CommandList::copy_acceleration_structure`] copies.
///
/// [`CommandList::copy_acceleration_structure`]: crate::CommandList::copy_acceleration_structure
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AccelerationStructureCopy {
    Clone,
    /// Into a structure of the compacted size the source reported.
    Compact,
}

// --- Sparse resources ---

/// Bytes in one tile of sparse memory.
pub const SPARSE_TILE_SIZE: u64 = 64 << 10;

/// How a sparse texture divides into tiles ([`Device::sparse_texture_layout`]).
///
/// [`Device::sparse_texture_layout`]: crate::Device::sparse_texture_layout
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct SparseTextureLayout {
    /// Texels per tile in x, y and z.
    pub tile_extent: [u32; 3],
    /// Tiles in the whole texture.
    pub tile_count: u32,
    /// Mips (from 0) that map tile by tile; the rest are packed.
    pub standard_mips: u32,
    /// Tiles the packed mips of one layer take, bound together
    /// ([`SparseTextureRegion::PackedMips`]); 0 without packed mips.
    pub packed_mip_tiles: u32,
}

/// Part of a sparse texture to bind.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SparseTextureRegion {
    /// A box of tiles in a standard mip: `origin` and `size` in tiles.
    Tiles {
        mip: u32,
        layer: u32,
        origin: [u32; 3],
        size: [u32; 3],
    },
    /// All packed mips of `layer`, as [`SparseTextureLayout::packed_mip_tiles`] tiles.
    PackedMips { layer: u32 },
}

/// Binds tiles of a sparse buffer or texture to heap memory, or unbinds them.
#[derive(Clone, Copy, Debug)]
pub enum SparseBind<'a> {
    Buffer {
        buffer: &'a crate::Buffer,
        /// In tiles of [`SPARSE_TILE_SIZE`].
        tile_offset: u32,
        tile_count: u32,
        /// The heap and its first tile; `None` unbinds.
        memory: Option<(&'a crate::Heap, u32)>,
    },
    Texture {
        texture: &'a crate::Texture,
        region: SparseTextureRegion,
        memory: Option<(&'a crate::Heap, u32)>,
    },
}

// --- Variable-rate shading ---

/// Pixels a pixel shader invocation covers (width × height).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum ShadingRate {
    #[default]
    R1x1,
    R1x2,
    R2x1,
    R2x2,
    R2x4,
    R4x2,
    R4x4,
}

impl ShadingRate {
    /// The rate as a shading-rate image texel holds it: `log2(width) << 2 | log2(height)`.
    pub const fn encoded(self) -> u8 {
        match self {
            Self::R1x1 => 0x0,
            Self::R1x2 => 0x1,
            Self::R2x1 => 0x4,
            Self::R2x2 => 0x5,
            Self::R2x4 => 0x6,
            Self::R4x2 => 0x9,
            Self::R4x4 => 0xa,
        }
    }
}

/// How two shading rates combine: first the draw's rate with the primitive's (`SV_ShadingRate`),
/// then that with the shading-rate image's.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum ShadingRateCombiner {
    /// The earlier rate.
    #[default]
    Keep,
    /// The later rate.
    Replace,
    Min,
    Max,
    Sum,
}
