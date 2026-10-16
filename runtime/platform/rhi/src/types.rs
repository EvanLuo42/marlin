//! Backend-neutral descriptions, shared by the public API and every backend.

use core::ffi::c_void;
use core::ops::{BitOr, BitOrAssign};
use core::ptr::NonNull;

use crate::{RenderTargetView, Texture};

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

/// Optional capabilities of a device, beyond the baseline (D3D12: feature level 12_0).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Features {
    /// Barriers run natively as sync/access/layout (D3D12 enhanced barriers, Vulkan
    /// synchronization2). Without it, D3D12 translates them into legacy resource states.
    pub enhanced_barriers: bool,
    /// [`PresentMode::Immediate`] can tear (variable refresh displays).
    pub tearing: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum QueueType {
    Graphics,
    Compute,
    Copy,
}

// --- Formats and textures ---

#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Format {
    Rgba8Unorm,
    Rgba8UnormSrgb,
    Bgra8Unorm,
    Bgra8UnormSrgb,
    Rgb10a2Unorm,
    Rgba16Float,
}

impl Format {
    pub const fn is_srgb(self) -> bool {
        matches!(self, Self::Rgba8UnormSrgb | Self::Bgra8UnormSrgb)
    }

    /// The same layout without sRGB encoding.
    pub const fn linear(self) -> Self {
        match self {
            Self::Rgba8UnormSrgb => Self::Rgba8Unorm,
            Self::Bgra8UnormSrgb => Self::Bgra8Unorm,
            other => other,
        }
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

/// What a render pass does with an attachment's existing contents.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LoadOp {
    Load,
    /// Clears to this RGBA value (linear; an sRGB target encodes it).
    Clear([f32; 4]),
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

#[derive(Clone, Copy, Debug, Default)]
pub struct RenderPassDesc<'a> {
    /// At most [`MAX_COLOR_ATTACHMENTS`]. Each must already be in [`Layout::RenderTarget`].
    pub color: &'a [ColorAttachment<'a>],
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
