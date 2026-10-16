//! Backend-neutral descriptions, shared by the public API and every backend.

use core::ffi::c_void;
use core::ops::{BitOr, BitOrAssign};
use core::ptr::NonNull;

use crate::{Buffer, DepthStencilView, RenderTargetView, Texture};

// --- Instance and adapters ---

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InstanceDesc {
    /// Enables the API's validation layer (D3D12 debug layer, Vulkan validation) when installed.
    /// Its messages go to the `marlin_diag` log. On D3D12 this is process-wide and permanent: the
    /// first instance that enables it enables it for every later device.
    pub validation: bool,
}

impl Default for InstanceDesc {
    fn default() -> Self {
        Self {
            validation: cfg!(debug_assertions),
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
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DeviceDesc {
    pub adapter: AdapterPreference,
    /// Use the legacy barrier path even where enhanced barriers exist. For testing the fallback.
    pub disable_enhanced_barriers: bool,
}

/// Optional capabilities of a device, beyond the baseline (D3D12: feature level 12_0, shader
/// model 6.6, resource binding tier 3).
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
    /// Inline ray queries (DXR 1.1), the `RayQuery` ext.
    pub ray_query: bool,
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
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Format {
    R8Unorm,
    Rg8Unorm,
    Rgba8Unorm,
    Rgba8UnormSrgb,
    Bgra8Unorm,
    Bgra8UnormSrgb,
    Rgb10a2Unorm,
    Rg11b10Float,
    R16Float,
    Rg16Float,
    Rgba16Float,
    R8Uint,
    R16Uint,
    R16Unorm,
    Rg16Uint,
    Rgba16Unorm,
    Rgba16Uint,
    R32Uint,
    R32Float,
    /// The visibility buffer: cluster and triangle ids.
    Rg32Uint,
    Rg32Float,
    Rgba32Uint,
    Rgba32Float,
    /// The depth format: reverse-Z wants float precision.
    D32Float,
    /// Depth with an 8-bit stencil.
    D32FloatS8,
    Bc1Unorm,
    Bc1UnormSrgb,
    Bc3Unorm,
    Bc3UnormSrgb,
    Bc4Unorm,
    Bc5Unorm,
    Bc6hUfloat,
    Bc6hSfloat,
    Bc7Unorm,
    Bc7UnormSrgb,
}

impl Format {
    pub const fn is_srgb(self) -> bool {
        matches!(
            self,
            Self::Rgba8UnormSrgb
                | Self::Bgra8UnormSrgb
                | Self::Bc1UnormSrgb
                | Self::Bc3UnormSrgb
                | Self::Bc7UnormSrgb
        )
    }

    /// The same layout without sRGB encoding.
    pub const fn linear(self) -> Self {
        match self {
            Self::Rgba8UnormSrgb => Self::Rgba8Unorm,
            Self::Bgra8UnormSrgb => Self::Bgra8Unorm,
            Self::Bc1UnormSrgb => Self::Bc1Unorm,
            Self::Bc3UnormSrgb => Self::Bc3Unorm,
            Self::Bc7UnormSrgb => Self::Bc7Unorm,
            other => other,
        }
    }

    pub const fn is_depth(self) -> bool {
        matches!(self, Self::D32Float | Self::D32FloatS8)
    }

    pub const fn has_stencil(self) -> bool {
        matches!(self, Self::D32FloatS8)
    }

    /// Integer formats: read and written as integers, never filtered or blended.
    pub const fn is_integer(self) -> bool {
        matches!(
            self,
            Self::R8Uint
                | Self::R16Uint
                | Self::Rg16Uint
                | Self::Rgba16Uint
                | Self::R32Uint
                | Self::Rg32Uint
                | Self::Rgba32Uint
        )
    }

    /// Block-compressed (4×4 texel blocks).
    pub const fn is_compressed(self) -> bool {
        self.block_extent() > 1
    }

    /// Width and height of a block in texels: 4 for block-compressed formats, else 1.
    pub const fn block_extent(self) -> u32 {
        match self {
            Self::Bc1Unorm
            | Self::Bc1UnormSrgb
            | Self::Bc3Unorm
            | Self::Bc3UnormSrgb
            | Self::Bc4Unorm
            | Self::Bc5Unorm
            | Self::Bc6hUfloat
            | Self::Bc6hSfloat
            | Self::Bc7Unorm
            | Self::Bc7UnormSrgb => 4,
            _ => 1,
        }
    }

    /// Bytes per block in buffer↔texture copies: [`block_size`](Self::block_size), except that
    /// depth-stencil formats copy only their 32-bit depth plane.
    pub const fn copy_block_size(self) -> u32 {
        match self {
            Self::D32FloatS8 => 4,
            other => other.block_size(),
        }
    }

    /// Bytes per block (per texel for uncompressed formats).
    pub const fn block_size(self) -> u32 {
        match self {
            Self::R8Unorm | Self::R8Uint => 1,
            Self::Rg8Unorm | Self::R16Float | Self::R16Uint | Self::R16Unorm => 2,
            Self::Rgba8Unorm
            | Self::Rgba8UnormSrgb
            | Self::Bgra8Unorm
            | Self::Bgra8UnormSrgb
            | Self::Rgb10a2Unorm
            | Self::Rg11b10Float
            | Self::Rg16Float
            | Self::R32Uint
            | Self::R32Float
            | Self::Rg16Uint
            | Self::D32Float => 4,
            Self::Rgba16Float
            | Self::Rgba16Unorm
            | Self::Rgba16Uint
            | Self::Rg32Float
            | Self::Rg32Uint
            | Self::D32FloatS8
            | Self::Bc1Unorm
            | Self::Bc1UnormSrgb => 8,
            Self::Bc4Unorm => 8,
            Self::Rgba32Float
            | Self::Rgba32Uint
            | Self::Bc3Unorm
            | Self::Bc3UnormSrgb
            | Self::Bc5Unorm
            | Self::Bc6hUfloat
            | Self::Bc6hSfloat
            | Self::Bc7Unorm
            | Self::Bc7UnormSrgb => 16,
        }
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
    /// GPU-written, CPU-read: results to read back. Mapped for the buffer's life.
    Readback,
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
}

impl TextureDesc {
    /// One layer, one mip, no clear value.
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

// --- Barriers ---

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
        INDIRECT = 0x40,
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

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TextureViewDimension {
    D2,
    D2Array,
    /// A volume texture. Writable views see every slice of one mip.
    D3,
    /// 6 layers starting at layer 0, in +X −X +Y −Y +Z −Z order.
    Cube,
}

/// A bindless view of a texture.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TextureViewDesc {
    /// Writable (requires [`TextureUsage::STORAGE`]; one mip) or read-only (requires
    /// [`TextureUsage::SAMPLED`]).
    pub writable: bool,
    pub dimension: TextureViewDimension,
    /// `None`: the texture's format (depth textures read as `R32Float`).
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
        }
    }
}

// --- Pipelines ---

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
}

impl DepthState {
    /// Reverse-Z (near = 1, far = 0) depth testing with writes.
    pub const fn reverse_z(format: Format) -> Self {
        Self {
            format,
            write: true,
            compare: CompareOp::GreaterEqual,
            stencil: None,
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

/// A graphics pipeline. There is no vertex input layout: vertex shaders fetch their vertices
/// from bindless buffers (vertex pulling). Index buffers stay fixed-function.
#[derive(Clone, Copy, Debug)]
pub struct GraphicsPipelineDesc<'a> {
    pub vertex: ShaderModule<'a>,
    /// `None`: depth-only.
    pub pixel: Option<ShaderModule<'a>>,
    pub topology: PrimitiveTopology,
    pub raster: RasterState,
    pub depth: Option<DepthState>,
    /// At most [`MAX_COLOR_ATTACHMENTS`]; formats must match the render pass's attachments.
    pub color_targets: &'a [ColorTargetState],
}

#[derive(Clone, Copy, Debug)]
pub struct ComputePipelineDesc<'a> {
    pub shader: ShaderModule<'a>,
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
        EXECUTE_INDIRECT = 0x800,
        ALL_SHADING = 0x1000,
        NON_PIXEL_SHADING = 0x2000,
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
        INDIRECT_ARGUMENT = 0x200,
        COPY_DST = 0x400,
        COPY_SRC = 0x800,
        RESOLVE_DST = 0x1000,
        RESOLVE_SRC = 0x2000,
    }
}

/// How a texture's memory is arranged. Maps 1:1 to D3D12 barrier layouts and Vulkan image
/// layouts; legacy D3D12 derives a resource state from it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Layout {
    /// Contents may be discarded. Only valid as a barrier's `before`. On legacy D3D12 the texture
    /// must actually be in its initial (common) state.
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
    /// Arguments and counts of indirect commands.
    pub const INDIRECT: Self = Self {
        stage: Stage::EXECUTE_INDIRECT,
        access: Access::INDIRECT_ARGUMENT,
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
}

impl<'a> TextureBarrier<'a> {
    /// A barrier over every subresource.
    pub const fn new(texture: &'a Texture, before: TextureState, after: TextureState) -> Self {
        Self {
            texture,
            before,
            after,
            range: SubresourceRange::ALL,
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
        }
    }
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

/// Alignment of placed resources' offsets that is always enough (64 KiB; D3D12's default).
pub const PLACEMENT_ALIGNMENT: u64 = 64 << 10;

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
