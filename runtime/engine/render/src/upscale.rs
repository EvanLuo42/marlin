//! Upscaling and anti-aliasing (0010 §7): a view is drawn at a render size with a subpixel
//! jitter a frame and resolved to the display size by an [`Upscaler`]. [`Tsr`], our temporal
//! super resolution, is every platform's baseline; vendors' (DLSS, FSR, XeSS, PSSR) implement the
//! same trait outside this crate. At [`UpscaleMode::Native`] the render and display sizes are one
//! and the upscaler is the anti-aliasing.
//!
//! A frame: the upscaler picks the render size ([`Upscaler::render_size`]) and this frame's
//! jitter ([`Upscaler::jitter`]), which goes into the camera ([`crate::Camera::jitter`], so every
//! pass draws and reconstructs with it, and motion vectors leave it out); the view is drawn; then
//! [`Upscaler::add_passes`] resolves it.

use marlin_math::scalar::{ceil, round};
use marlin_math::{UVec2, Vec2};
use marlin_render_graph::{
    Access, BufferHandle, PassContext, RenderGraph, TextureDesc as GraphTextureDesc, TextureHandle,
};
use marlin_rhi::{
    AddressMode, ComputePipeline, ComputePipelineDesc, Device, Format, Result, Sampler,
    SamplerDesc, ShaderModule,
};
use marlin_serial::cooked::Pod;
use marlin_shader::asset::ShaderLibrary;
use marlin_shader::bind::{BindError, EntryDesc, bind};

/// How much smaller than the display the view is drawn (per axis).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum UpscaleMode {
    /// The display size: the upscaler only anti-aliases. The desktop default (0010 §7.1).
    #[default]
    Native,
    /// 1.5×.
    Quality,
    /// 1.7×.
    Balanced,
    /// 2×.
    Performance,
    /// 3×.
    UltraPerformance,
    /// Any ratio of at least 1.
    Ratio(f32),
}

impl UpscaleMode {
    /// Display pixels per render pixel, per axis.
    pub fn ratio(self) -> f32 {
        match self {
            Self::Native => 1.0,
            Self::Quality => 1.5,
            Self::Balanced => 1.7,
            Self::Performance => 2.0,
            Self::UltraPerformance => 3.0,
            Self::Ratio(r) => r.max(1.0),
        }
    }
}

/// What an upscaler can do.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct UpscalerCaps {
    /// The largest [`UpscaleMode::ratio`] it takes.
    pub max_ratio: f32,
    /// It denoises too (DLSS Ray Reconstruction): the graph then declares no denoiser and hands
    /// it the noisy signals (0010 §6).
    pub denoises: bool,
    /// It reads a reactive mask (0009 §8).
    pub reactive_mask: bool,
}

/// One view's frame, for [`Upscaler::add_passes`].
#[derive(Clone, Copy)]
pub struct UpscaleInputs<'g> {
    /// The HDR color, depth (reverse-Z) and motion vectors at the render size.
    pub color: TextureHandle<'g>,
    pub depth: TextureHandle<'g>,
    pub velocity: TextureHandle<'g>,
    /// The post chain's exposure buffer (`PostProcess::exposure`): last frame's value.
    pub exposure: BufferHandle<'g>,
    /// This frame's jitter, in render pixels ([`Upscaler::jitter`]).
    pub jitter: Vec2,
    pub render: UVec2,
    pub display: UVec2,
}

/// An upscaler (0010 §7.2).
pub trait Upscaler: Send + Sync {
    fn caps(&self) -> UpscalerCaps;
    /// The size to draw a `display`-sized view at in `mode`.
    fn render_size(&self, display: UVec2, mode: UpscaleMode) -> UVec2;
    /// Frame `frame`'s jitter, in render pixels (x right, y down, within ±0.5).
    fn jitter(&self, frame: u64, render: UVec2, display: UVec2) -> Vec2;
    /// Declares the resolve of `view`'s frame; returns its HDR color at the display size.
    fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        view: u64,
        inputs: &UpscaleInputs<'g>,
    ) -> TextureHandle<'g>;
}

/// TSR's shaders (`Marlin.Engine.Post`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TsrShaders<'a> {
    pub resolve: ShaderModule<'a>,
}

impl<'a> TsrShaders<'a> {
    pub const ENTRIES: [EntryDesc; 1] =
        [EntryDesc::compute("Marlin.Engine.Post.TsrResolve").push::<TsrArgs>()];

    pub fn from_modules([resolve]: [ShaderModule<'a>; 1]) -> Self {
        Self { resolve }
    }

    pub fn bind(library: &'a ShaderLibrary) -> core::result::Result<Self, BindError> {
        bind(library, &Self::ENTRIES).map(Self::from_modules)
    }
}

/// `Marlin.Engine.Post.TsrConstants`.
#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct Constants {
    render_size: [u32; 2],
    display_size: [u32; 2],
    jitter: [f32; 2],
    has_history: u32,
    max_weight: f32,
    clip_sigma: f32,
    sharpness: f32,
    motion_falloff: f32,
    spare: u32,
}

/// `Marlin.Engine.Post.TsrArgs`.
#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct TsrArgs {
    constants: u32,
    color: u32,
    depth: u32,
    velocity: u32,
    history: u32,
    exposure: u32,
    linear: u32,
    output: u32,
}

/// The accumulation at the display size: HDR color and the weight behind it.
const HISTORY_FORMAT: Format = Format::Rgba16Float;

/// Temporal super resolution (0010 §7.1; `Post/Tsr.mshader`, whose file comment describes the
/// pass).
pub struct Tsr {
    resolve: ComputePipeline,
    linear: Sampler,
    /// The most samples' weight the accumulation keeps at the native size; upscaling scales it
    /// by the ratio squared (a display pixel gets that many times fewer samples a frame).
    pub max_weight: f32,
    /// How far from the neighborhood's mean the history may be, in its deviations.
    pub clip_sigma: f32,
    /// The reconstruction filter: `exp(-sharpness · d²)`, d in render pixels.
    pub sharpness: f32,
    /// How fast the history's weight falls with motion (display pixels a frame): resampling a
    /// moving history softens it, so a moving one keeps fewer frames.
    pub motion_falloff: f32,
}

impl Tsr {
    /// Defaults: a weight of 16 (about 16 frames at the native size) that falls with motion,
    /// clipping at 1.25 deviations, a filter of `exp(-6 d²)` in display pixels (about a display
    /// pixel's box: measured closest to supersampling, 0010 §7.4).
    pub fn new(device: &Device, shaders: &TsrShaders<'_>) -> Result<Self> {
        Ok(Self {
            resolve: device.create_compute_pipeline(&ComputePipelineDesc {
                shader: shaders.resolve,
                cache: None,
            })?,
            linear: device.create_sampler(&SamplerDesc {
                address_u: AddressMode::ClampToEdge,
                address_v: AddressMode::ClampToEdge,
                address_w: AddressMode::ClampToEdge,
                ..SamplerDesc::default()
            })?,
            max_weight: 16.0,
            clip_sigma: 1.25,
            sharpness: 6.0,
            motion_falloff: 2.0,
        })
    }

    /// Jitter phases for an upscale `ratio`: more as each display pixel gets fewer samples.
    pub fn phases(ratio: f32) -> u64 {
        (round(8.0 * ratio * ratio) as u64).clamp(8, 32)
    }
}

/// The radical inverse of `i` in `base`: the Halton sequence.
fn halton(mut i: u64, base: u64) -> f32 {
    let (mut f, mut r) = (1.0f32, 0.0f32);
    while i > 0 {
        f /= base as f32;
        r += f * (i % base) as f32;
        i /= base;
    }
    r
}

impl Upscaler for Tsr {
    fn caps(&self) -> UpscalerCaps {
        UpscalerCaps {
            max_ratio: 3.0,
            denoises: false,
            reactive_mask: false,
        }
    }

    fn render_size(&self, display: UVec2, mode: UpscaleMode) -> UVec2 {
        let ratio = mode.ratio().min(self.caps().max_ratio);
        UVec2::new(
            (ceil(display.x as f32 / ratio) as u32).max(1),
            (ceil(display.y as f32 / ratio) as u32).max(1),
        )
    }

    fn jitter(&self, frame: u64, render: UVec2, display: UVec2) -> Vec2 {
        let ratio = display.y as f32 / render.y.max(1) as f32;
        let i = frame % Self::phases(ratio) + 1;
        Vec2::new(halton(i, 2) - 0.5, halton(i, 3) - 0.5)
    }

    fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        view: u64,
        inputs: &UpscaleInputs<'g>,
    ) -> TextureHandle<'g> {
        let UpscaleInputs {
            color,
            depth,
            velocity,
            exposure,
            jitter,
            render,
            display,
        } = *inputs;
        let history = g.history(
            "TsrHistory",
            view,
            GraphTextureDesc::new_2d(display.x, display.y, HISTORY_FORMAT),
        );
        let ratio = display.y as f32 / render.y.max(1) as f32;
        let constants = Constants {
            render_size: [render.x, render.y],
            display_size: [display.x, display.y],
            jitter: [jitter.x, jitter.y],
            has_history: u32::from(history.prev.is_some()),
            max_weight: (self.max_weight * ratio * ratio).min(64.0),
            clip_sigma: self.clip_sigma,
            sharpness: self.sharpness,
            motion_falloff: self.motion_falloff,
            spare: 0,
        };
        let linear = self.linear.index();
        g.add_compute("TsrResolve", |b| {
            let color = b.read_texture(color, Access::ComputeRead);
            let depth = b.read_texture(depth, Access::ComputeRead);
            let velocity = b.read_texture(velocity, Access::ComputeRead);
            let exposure = b.read_buffer(exposure, Access::ComputeRead);
            let previous = history.prev.map(|h| b.read_texture(h, Access::ComputeRead));
            let output = b.write_texture(history.cur, Access::ComputeWrite);
            (output, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.resolve);
                let args = TsrArgs {
                    constants: ctx.upload(&constants),
                    color: ctx.srv(color),
                    depth: ctx.srv(depth),
                    velocity: ctx.srv(velocity),
                    // Without history it goes unread: the color stands in.
                    history: previous.map_or_else(|| ctx.srv(color), |h| ctx.srv(h)),
                    exposure: ctx.srv(exposure),
                    linear,
                    output: ctx.uav(output),
                };
                ctx.push(&args);
                ctx.dispatch(display.x.div_ceil(8), display.y.div_ceil(8), 1);
            })
        })
    }
}
