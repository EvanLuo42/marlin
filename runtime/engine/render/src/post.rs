//! Post-processing (0007 §11.2): the HDR scene's bloom, its exposure and the tonemap into the
//! output. Bloom is a chain of half-size downsamples and upsamples, each a transient of its own
//! (they alias as the chain goes). The luminance histogram and the exposure it gives run on the
//! compute queue while the graphics queue blooms; the exposure adapts over frames in a
//! persistent buffer per view.

use alloc::vec::Vec;

use marlin_render_graph::{
    Access, BufferDesc as GraphBufferDesc, BufferHandle, PassContext, RenderGraph,
    TextureDesc as GraphTextureDesc, TextureHandle,
};
use marlin_rhi::{
    AddressMode, ColorTargetState, ComputePipeline, ComputePipelineDesc, CullMode, Device, Format,
    GraphicsPipeline, GraphicsPipelineDesc, LoadOp, MultisampleState, PrimitiveTopology,
    RasterState, Result, Sampler, SamplerDesc, ShaderModule, StoreOp,
};
use marlin_serial::cooked::Pod;
use marlin_shader::asset::ShaderLibrary;
use marlin_shader::bind::{BindError, EntryDesc, bind};
use marlin_shader::reflect::Stage;

/// The format of the HDR scene the post chain reads: what the lighting writes.
pub const HDR_FORMAT: Format = Format::Rgba16Float;

/// The post chain's shaders (`Marlin.Engine.Post`), from the cooker like every engine shader.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PostShaders<'a> {
    pub bloom_down: ShaderModule<'a>,
    pub bloom_up: ShaderModule<'a>,
    pub histogram: ShaderModule<'a>,
    pub exposure: ShaderModule<'a>,
    pub fullscreen: ShaderModule<'a>,
    pub tonemap: ShaderModule<'a>,
}

impl<'a> PostShaders<'a> {
    /// The entries, in field order.
    pub const ENTRIES: [EntryDesc; 6] = [
        EntryDesc::compute("Marlin.Engine.Post.BloomDown").push::<BloomDownArgs>(),
        EntryDesc::compute("Marlin.Engine.Post.BloomUp").push::<BloomUpArgs>(),
        EntryDesc::compute("Marlin.Engine.Post.Histogram").push::<HistogramArgs>(),
        EntryDesc::compute("Marlin.Engine.Post.Exposure").push::<ExposureArgs>(),
        EntryDesc::new("Marlin.Engine.Post.FullscreenVs", Stage::Vertex),
        EntryDesc::new("Marlin.Engine.Post.TonemapPs", Stage::Pixel).push::<TonemapArgs>(),
    ];

    /// The shaders from `modules`, in [`ENTRIES`](Self::ENTRIES) order.
    pub fn from_modules(
        [
            bloom_down,
            bloom_up,
            histogram,
            exposure,
            fullscreen,
            tonemap,
        ]: [ShaderModule<'a>; 6],
    ) -> Self {
        Self {
            bloom_down,
            bloom_up,
            histogram,
            exposure,
            fullscreen,
            tonemap,
        }
    }

    /// The post chain's shaders from `library`, checked against what it sets.
    pub fn bind(library: &'a ShaderLibrary) -> core::result::Result<Self, BindError> {
        bind(library, &Self::ENTRIES).map(Self::from_modules)
    }
}

/// How the post chain looks.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PostSettings {
    /// Bloom's share of the light.
    pub bloom_intensity: f32,
    /// Most bloom levels (half sizes); fewer when the target is small.
    pub bloom_levels: u32,
    /// Texels of the smaller level the upsample tent spans.
    pub bloom_radius: f32,
    /// Stops added to the automatic exposure.
    pub exposure_compensation: f32,
    /// How fast the exposure follows the scene: the rate of an exponential, per second.
    pub adaptation_speed: f32,
    /// The log2 luminance range the histogram covers.
    pub min_log_luminance: f32,
    pub max_log_luminance: f32,
    /// The average log luminance takes the pixels from `low_fraction` of them, darkest first,
    /// to `high_fraction`: the middle, so neither a dark sky nor a few highlights sway it.
    pub low_fraction: f32,
    pub high_fraction: f32,
}

impl Default for PostSettings {
    fn default() -> Self {
        Self {
            bloom_intensity: 0.04,
            bloom_levels: 6,
            bloom_radius: 1.0,
            exposure_compensation: 0.0,
            adaptation_speed: 1.5,
            min_log_luminance: -12.0,
            max_log_luminance: 10.0,
            low_fraction: 0.1,
            high_fraction: 0.9,
        }
    }
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct BloomDownArgs {
    source: u32,
    linear: u32,
    target: u32,
    first: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct BloomUpArgs {
    low: u32,
    high: u32,
    linear: u32,
    target: u32,
    radius: f32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct HistogramArgs {
    scene: u32,
    histogram: u32,
    min_log_luminance: f32,
    inverse_log_luminance_range: f32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct ExposureArgs {
    histogram: u32,
    exposure: u32,
    pixels: u32,
    min_log_luminance: f32,
    log_luminance_range: f32,
    adapt: f32,
    fresh: u32,
    compensation: f32,
    low_fraction: f32,
    high_fraction: f32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct TonemapArgs {
    scene: u32,
    bloom: u32,
    linear: u32,
    exposure: u32,
    bloom_intensity: f32,
}

/// Bins of the luminance histogram (the shader's).
const BINS: u64 = 256;

/// The post chain's pipelines and sampler.
pub struct PostProcess {
    bloom_down: ComputePipeline,
    bloom_up: ComputePipeline,
    histogram: ComputePipeline,
    exposure: ComputePipeline,
    tonemap: GraphicsPipeline,
    linear: Sampler,
    pub settings: PostSettings,
}

/// What [`PostProcess::add_passes`] declared.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PostOutput<'g> {
    /// The output, tonemapped.
    pub output: TextureHandle<'g>,
    /// The bloom at half size, before the tonemap adds it.
    pub bloom: TextureHandle<'g>,
    /// The adapted log2 luminance (f32) and the exposure (f32) the tonemap used.
    pub exposure: BufferHandle<'g>,
}

impl PostProcess {
    /// Pipelines for an output of `output_format`.
    pub fn new(device: &Device, shaders: &PostShaders<'_>, output_format: Format) -> Result<Self> {
        let compute = |shader| {
            device.create_compute_pipeline(&ComputePipelineDesc {
                shader,
                cache: None,
            })
        };
        Ok(Self {
            bloom_down: compute(shaders.bloom_down)?,
            bloom_up: compute(shaders.bloom_up)?,
            histogram: compute(shaders.histogram)?,
            exposure: compute(shaders.exposure)?,
            tonemap: device.create_graphics_pipeline(&GraphicsPipelineDesc {
                vertex: shaders.fullscreen,
                pixel: Some(shaders.tonemap),
                topology: PrimitiveTopology::TriangleList,
                // One triangle over the target, whichever way it winds.
                raster: RasterState {
                    cull: CullMode::None,
                    ..RasterState::default()
                },
                depth: None,
                color_targets: &[ColorTargetState::new(output_format)],
                multisample: MultisampleState::NONE,
                primitive_restart: None,
                cache: None,
            })?,
            linear: device.create_sampler(&SamplerDesc {
                address_u: AddressMode::ClampToEdge,
                address_v: AddressMode::ClampToEdge,
                address_w: AddressMode::ClampToEdge,
                ..SamplerDesc::default()
            })?,
            settings: PostSettings::default(),
        })
    }

    /// Declares the chain: bloom of `scene` (an [`HDR_FORMAT`] texture), its exposure on the
    /// compute queue, and the tonemap into `output` (overwritten). `view` keys the exposure that
    /// adapts across frames; `dt` is the frame's time in seconds (infinite: the exposure takes the
    /// scene's at once). Needs a graph with a state table (the exposure is a persistent resource).
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        scene: TextureHandle<'g>,
        output: TextureHandle<'g>,
        view: u64,
        dt: f32,
    ) -> PostOutput<'g> {
        let s = self.settings;
        let size = *g.texture_desc(scene);
        let (width, height) = (size.width, size.height);
        let linear = self.linear.index();

        // Bloom: downsample to ever smaller levels, then add each level's blur back up.
        let smallest_side = width.min(height).max(1);
        let levels = s.bloom_levels.min(smallest_side.ilog2()).max(1);
        let mut downs: Vec<TextureHandle<'g>> = Vec::with_capacity(levels as usize);
        let mut source = scene;
        for level in 0..levels {
            let (w, h) = (
                (width >> (level + 1)).max(1),
                (height >> (level + 1)).max(1),
            );
            let target = g.create_texture("BloomDown", GraphTextureDesc::new_2d(w, h, HDR_FORMAT));
            let first = u32::from(level == 0);
            let pipeline = &self.bloom_down;
            let down = g.add_compute("BloomDown", |b| {
                let src = b.read_texture(source, Access::ComputeRead);
                let dst = b.write_texture(target, Access::ComputeWrite);
                (dst, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(pipeline);
                    ctx.push(&BloomDownArgs {
                        source: ctx.srv(src),
                        linear,
                        target: ctx.uav(dst),
                        first,
                    });
                    ctx.dispatch(w.div_ceil(8), h.div_ceil(8), 1);
                })
            });
            downs.push(down);
            source = down;
        }
        let mut low = downs[levels as usize - 1];
        for level in (0..levels - 1).rev() {
            let high = downs[level as usize];
            let (w, h) = (
                (width >> (level + 1)).max(1),
                (height >> (level + 1)).max(1),
            );
            let target = g.create_texture("BloomUp", GraphTextureDesc::new_2d(w, h, HDR_FORMAT));
            let pipeline = &self.bloom_up;
            let radius = s.bloom_radius;
            low = g.add_compute("BloomUp", |b| {
                let l = b.read_texture(low, Access::ComputeRead);
                let hi = b.read_texture(high, Access::ComputeRead);
                let dst = b.write_texture(target, Access::ComputeWrite);
                (dst, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(pipeline);
                    ctx.push(&BloomUpArgs {
                        low: ctx.srv(l),
                        high: ctx.srv(hi),
                        linear,
                        target: ctx.uav(dst),
                        radius,
                    });
                    ctx.dispatch(w.div_ceil(8), h.div_ceil(8), 1);
                })
            });
        }
        let bloom = low;

        // Exposure, on the compute queue.
        let range = s.max_log_luminance - s.min_log_luminance;
        let histogram = g.create_buffer(
            "LuminanceHistogram",
            GraphBufferDesc::structured::<u32>(BINS),
        );
        let histogram = g.add_compute("LuminanceClear", |b| {
            b.async_compute();
            let h = b.write_buffer(histogram, Access::Clear);
            (h, move |ctx: &mut PassContext<'_>| ctx.fill_buffer(h, 0))
        });
        let pipeline = &self.histogram;
        let histogram = g.add_compute("LuminanceHistogram", |b| {
            b.async_compute();
            let src = b.read_texture(scene, Access::ComputeRead);
            let h = b.write_buffer(histogram, Access::ComputeWrite);
            (h, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(pipeline);
                ctx.push(&HistogramArgs {
                    scene: ctx.srv(src),
                    histogram: ctx.uav(h),
                    min_log_luminance: s.min_log_luminance,
                    inverse_log_luminance_range: 1.0 / range,
                });
                ctx.dispatch(width.div_ceil(16), height.div_ceil(16), 1);
            })
        });
        let exposure = g.persistent("Exposure", view, GraphBufferDesc::new(8));
        let fresh = u32::from(exposure.fresh);
        let adapt = if dt.is_infinite() {
            1.0
        } else {
            1.0 - marlin_math::scalar::exp(-dt.max(0.0) * s.adaptation_speed)
        };
        let pipeline = &self.exposure;
        let exposure = g.add_compute("Exposure", |b| {
            b.async_compute();
            let h = b.read_buffer(histogram, Access::ComputeRead);
            let e = b.write_buffer(exposure.handle, Access::ComputeWrite);
            (e, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(pipeline);
                ctx.push(&ExposureArgs {
                    histogram: ctx.srv(h),
                    exposure: ctx.uav(e),
                    pixels: width * height,
                    min_log_luminance: s.min_log_luminance,
                    log_luminance_range: range,
                    adapt,
                    fresh,
                    compensation: s.exposure_compensation,
                    low_fraction: s.low_fraction,
                    high_fraction: s.high_fraction,
                });
                ctx.dispatch(1, 1, 1);
            })
        });

        // Tonemap into the output.
        let pipeline = &self.tonemap;
        let bloom_intensity = s.bloom_intensity / levels as f32;
        let output = g.add_raster("Tonemap", |b| {
            let out = b.color(output, LoadOp::DontCare, StoreOp::Store);
            let sc = b.read_texture(scene, Access::PixelRead);
            let bl = b.read_texture(bloom, Access::PixelRead);
            let ex = b.read_buffer(exposure, Access::PixelRead);
            (out, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(pipeline);
                ctx.push(&TonemapArgs {
                    scene: ctx.srv(sc),
                    bloom: ctx.srv(bl),
                    linear,
                    exposure: ctx.srv(ex),
                    bloom_intensity,
                });
                ctx.draw(3, 1, 0, 0);
            })
        });
        PostOutput {
            output,
            bloom,
            exposure,
        }
    }
}
