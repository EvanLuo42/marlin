//! The GPU side (0012 §5): the passes of one surface, on the render graph.
//!
//! [`UiRenderer::prepare`] is the CPU part of a frame: the draw list's glyphs made resident in
//! the glyph cache (new ones rasterized), the list flattened into one command buffer, the glyph
//! table built. [`UiRenderer::add_passes`] declares the passes: the new glyphs into the cache's
//! pages, then bounds, coarse and fine binning and shading into a UI target, which
//! [`UiRenderer::encode`] writes into the window's image.
//!
//! The tile lists come from a pool of blocks sized from what earlier frames used: each frame
//! copies its count into a small readback buffer, read when its frame slot comes round again.
//! A frame that runs out of blocks drops commands from the tiles it could not extend (and says
//! so through [`UiRenderer::pool_overflowed`]); the next frames get twice the pool.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use marlin_font::{FontView, GlyphCache, GlyphCacheConfig, GlyphRecord, GlyphUpload, PrepareStats};
use marlin_jobs::JobSystem;
use marlin_render_graph::{
    Access, BufferDesc, BufferHandle, FRAMES_IN_FLIGHT, FrameContext, PassContext, RenderGraph,
    TextureDesc, TextureHandle,
};
use marlin_rhi::{
    AddressMode, Buffer, BufferUsage, ColorTargetState, ComputePipeline, ComputePipelineDesc,
    CullMode, Device, Error, ErrorKind, Format, FormatFeatures, GraphicsPipeline,
    GraphicsPipelineDesc, LoadOp, MemoryLocation, MultisampleState, PrimitiveTopology, RasterState,
    Result, Sampler, SamplerDesc, ShaderModule, StoreOp, Texture, TextureUsage,
};
use marlin_serial::cooked::Pod;

use crate::command::{
    BIN, BLOCK_WORDS, CHUNK_COMMANDS, Chunk, DEFAULT_SAMPLER, Kind, PAINT_IMAGE, TILE,
};
use crate::list::UiDrawList;
use crate::style::Color;

/// The format of the UI target: sRGB-encoded values in a linear format, blended as they are
/// (§5.6, §8.1).
pub const UI_FORMAT: Format = Format::Rgba8Unorm;

/// The UI renderer's shaders (`Marlin.Engine.Ui`).
pub struct UiShaders<'a> {
    pub glyph_upload: ShaderModule<'a>,
    pub bounds: ShaderModule<'a>,
    pub bin: ShaderModule<'a>,
    pub coarse: ShaderModule<'a>,
    pub fine: ShaderModule<'a>,
    pub encode_vs: ShaderModule<'a>,
    pub encode_ps: ShaderModule<'a>,
}

/// The entries, in [`UiShaders`] order.
pub const UI_ENTRIES: [&str; 7] = [
    "Marlin.Engine.Ui.GlyphUpload",
    "Marlin.Engine.Ui.Bounds",
    "Marlin.Engine.Ui.Bin",
    "Marlin.Engine.Ui.Coarse",
    "Marlin.Engine.Ui.Fine",
    "Marlin.Engine.Ui.EncodeVs",
    "Marlin.Engine.Ui.EncodePs",
];

/// Push constant bytes of each entry, in [`UI_ENTRIES`] order (the encode vertex shader has
/// none).
pub const UI_PUSH_BYTES: [Option<u32>; 7] = [
    Some(size_of::<GlyphUploadArgs>() as u32),
    Some(size_of::<BoundsArgs>() as u32),
    Some(size_of::<BinArgs>() as u32),
    Some(size_of::<CoarseArgs>() as u32),
    Some(size_of::<FineArgs>() as u32),
    None,
    Some(size_of::<EncodeArgs>() as u32),
];

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct UiFrame {
    width: u32,
    height: u32,
    commands: u32,
    chunks: u32,
    bins_x: u32,
    bins_y: u32,
    tiles_x: u32,
    tiles_y: u32,
    pool_blocks: u32,
    spare: [u32; 3],
    clear: [f32; 4],
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct GlyphUploadArgs {
    uploads: u32,
    data: u32,
    pages: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct BoundsArgs {
    frame: u32,
    commands: u32,
    payload: u32,
    glyphs: u32,
    bounds: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct BinArgs {
    frame: u32,
    bounds: u32,
    masks: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct CoarseArgs {
    frame: u32,
    commands: u32,
    payload: u32,
    bounds: u32,
    masks: u32,
    heads: u32,
    pool: u32,
    counters: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct FineArgs {
    frame: u32,
    commands: u32,
    payload: u32,
    paints: u32,
    glyphs: u32,
    heads: u32,
    pool: u32,
    glyph_pages: u32,
    output: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct EncodeArgs {
    ui: u32,
    decode: u32,
}

/// How a [`UiRenderer`] is set up.
#[derive(Clone, Copy, Debug)]
pub struct UiRendererDesc {
    pub glyph_cache: GlyphCacheConfig,
    /// The format [`UiRenderer::encode`] writes (the swapchain's); `None` for a renderer that
    /// only draws UI targets.
    pub output_format: Option<Format>,
    /// Tile list blocks the first frames get (128 bytes each).
    pub initial_pool_blocks: u32,
}

impl Default for UiRendererDesc {
    fn default() -> Self {
        Self {
            glyph_cache: GlyphCacheConfig::default(),
            output_format: None,
            initial_pool_blocks: 16384,
        }
    }
}

/// A frame's CPU work, ready for [`UiRenderer::add_passes`].
pub struct UiFrameData {
    chunk: Chunk,
    glyphs: Vec<GlyphRecord>,
    uploads: Vec<GlyphUpload>,
    pub stats: PrepareStats,
}

impl UiFrameData {
    /// Every command of the frame, as the GPU gets them.
    pub fn commands(&self) -> &Chunk {
        &self.chunk
    }

    /// The glyph table the frame draws with.
    pub fn glyph_table(&self) -> &[GlyphRecord] {
        &self.glyphs
    }
}

/// What [`UiRenderer::add_passes`] declared.
#[derive(Clone, Copy, Debug)]
pub struct UiOutput<'g> {
    /// The UI target: [`UI_FORMAT`], sRGB-encoded, premultiplied.
    pub target: TextureHandle<'g>,
    /// Per tile (row by row): its list's first block and length (two words).
    pub heads: BufferHandle<'g>,
    /// The blocks: [`BLOCK_WORDS`] words each, the last one the next block.
    pub pool: BufferHandle<'g>,
}

/// One surface's UI renderer: pipelines, the glyph cache and its pages.
pub struct UiRenderer {
    glyph_upload: ComputePipeline,
    bounds: ComputePipeline,
    bin: ComputePipeline,
    coarse: ComputePipeline,
    fine: ComputePipeline,
    encode: Option<(GraphicsPipeline, bool)>,
    sampler: Sampler,
    glyphs: GlyphCache,
    pages: Texture,
    /// The pool's size this frame, raised from what frames used.
    pool_blocks: AtomicU32,
    overflowed: AtomicBool,
    readbacks: Vec<Buffer>,
}

impl UiRenderer {
    pub fn new(device: &Device, shaders: &UiShaders<'_>, desc: &UiRendererDesc) -> Result<Self> {
        if !device
            .format_support(Format::R8Unorm)
            .features
            .contains(FormatFeatures::STORAGE_WRITE)
        {
            return Err(Error::from_kind(ErrorKind::Unsupported));
        }
        let compute = |shader| {
            device.create_compute_pipeline(&ComputePipelineDesc {
                shader,
                cache: None,
            })
        };
        let encode = match desc.output_format {
            None => None,
            Some(format) => Some((
                device.create_graphics_pipeline(&GraphicsPipelineDesc {
                    vertex: shaders.encode_vs,
                    pixel: Some(shaders.encode_ps),
                    topology: PrimitiveTopology::TriangleList,
                    raster: RasterState {
                        cull: CullMode::None,
                        ..RasterState::default()
                    },
                    depth: None,
                    color_targets: &[ColorTargetState::new(format)],
                    multisample: MultisampleState::NONE,
                    primitive_restart: None,
                    cache: None,
                })?,
                format.is_srgb(),
            )),
        };
        let size = desc.glyph_cache.page_size;
        // At least two layers, so the view is an array whatever the page count.
        let layers = desc.glyph_cache.max_pages.max(2);
        let pages = device.create_texture(&marlin_rhi::TextureDesc {
            array_layers: layers,
            ..marlin_rhi::TextureDesc::new_2d(
                size,
                size,
                Format::R8Unorm,
                TextureUsage::SAMPLED | TextureUsage::STORAGE,
            )
        })?;
        let readbacks = (0..FRAMES_IN_FLIGHT)
            .map(|_| {
                device.create_buffer(&marlin_rhi::BufferDesc {
                    size: 8,
                    usage: BufferUsage::COPY_DST,
                    memory: MemoryLocation::Readback,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            glyph_upload: compute(shaders.glyph_upload)?,
            bounds: compute(shaders.bounds)?,
            bin: compute(shaders.bin)?,
            coarse: compute(shaders.coarse)?,
            fine: compute(shaders.fine)?,
            encode,
            sampler: device.create_sampler(&SamplerDesc {
                address_u: AddressMode::ClampToEdge,
                address_v: AddressMode::ClampToEdge,
                address_w: AddressMode::ClampToEdge,
                ..SamplerDesc::default()
            })?,
            glyphs: GlyphCache::new(desc.glyph_cache),
            pages,
            pool_blocks: AtomicU32::new(desc.initial_pool_blocks.max(64)),
            overflowed: AtomicBool::new(false),
            readbacks,
        })
    }

    /// The glyph cache, whose ids painters write ([`UiDrawList::painter`]).
    pub fn glyph_cache(&self) -> &GlyphCache {
        &self.glyphs
    }

    /// Whether a frame ran out of tile list blocks since the last call.
    pub fn pool_overflowed(&self) -> bool {
        self.overflowed.swap(false, Ordering::Relaxed)
    }

    /// Tile list blocks the next frame gets.
    pub fn pool_blocks(&self) -> u32 {
        self.pool_blocks.load(Ordering::Relaxed)
    }

    /// The CPU part of a frame: `list`'s glyphs made resident (`fonts[i]` is `FontId(i)`),
    /// rasterizing new ones on `jobs`, and its commands flattened.
    pub fn prepare(
        &mut self,
        list: &UiDrawList,
        fonts: &[FontView<'_>],
        jobs: Option<&JobSystem>,
    ) -> UiFrameData {
        let mut chunk = list.flatten();
        let sampler = self.sampler.index();
        for p in &mut chunk.paints {
            if p[0] == PAINT_IMAGE && p[3] == DEFAULT_SAMPLER {
                p[3] = sampler;
            }
        }
        let used: Vec<_> = chunk
            .commands
            .iter()
            .filter(|c| Kind::from_word(c[0]) == Some(Kind::Glyph))
            .map(|c| marlin_font::GlyphId(chunk.payload[c[3] as usize + 2]))
            .collect();
        let stats = self.glyphs.prepare(fonts, &used, jobs);
        UiFrameData {
            chunk,
            glyphs: self.glyphs.table(),
            uploads: self.glyphs.take_uploads(),
            stats,
        }
    }

    /// Declares the frame's UI passes for a `width` × `height` surface over `clear`, and
    /// returns the UI target. Needs a graph with a state table (the frame context's).
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        data: &'g UiFrameData,
        width: u32,
        height: u32,
        clear: Color,
    ) -> UiOutput<'g> {
        let commands = data.chunk.commands.len() as u32;
        let chunks = commands.div_ceil(CHUNK_COMMANDS);
        let (bins_x, bins_y) = (width.div_ceil(BIN), height.div_ceil(BIN));
        let (tiles_x, tiles_y) = (width.div_ceil(TILE), height.div_ceil(TILE));
        let pool_blocks = self.pool_blocks.load(Ordering::Relaxed);
        let c = crate::shape::premultiplied(clear.packed());
        let frame = UiFrame {
            width,
            height,
            commands,
            chunks,
            bins_x,
            bins_y,
            tiles_x,
            tiles_y,
            pool_blocks,
            spare: [0; 3],
            clear: c,
        };
        // Uploads need at least one element.
        let one = |s: &'g [[u32; 4]]| if s.is_empty() { &[[0u32; 4]][..] } else { s };
        let cmds: &'g [[u32; 4]] = one(&data.chunk.commands);
        let paints: &'g [[u32; 4]] = one(&data.chunk.paints);
        let glyphs: &'g [[u32; 4]] = one(&data.glyphs);
        let payload: &'g [u32] = if data.chunk.payload.is_empty() {
            &[0]
        } else {
            &data.chunk.payload
        };

        let pages = g.import_texture("UiGlyphPages", &self.pages);
        let pages = if data.uploads.is_empty() {
            pages
        } else {
            self.upload_glyphs(g, &data.uploads, pages)
        };

        let bounds = g.create_buffer(
            "UiBounds",
            BufferDesc::structured::<[u32; 4]>(u64::from(commands.max(1))),
        );
        let masks = g.create_buffer(
            "UiMasks",
            BufferDesc::structured::<u32>(u64::from(chunks.max(1) * bins_x * bins_y * 8)),
        );
        let heads = g.create_buffer(
            "UiTileHeads",
            BufferDesc::structured::<[u32; 2]>(u64::from(tiles_x * tiles_y)),
        );
        let pool = g.create_buffer(
            "UiTilePool",
            BufferDesc::structured::<u32>(u64::from(pool_blocks * BLOCK_WORDS)),
        );
        let counters = g.create_buffer("UiCounters", BufferDesc::structured::<u32>(2));

        let (masks, counters) = g.add_compute("UiClear", |b| {
            let m = b.write_buffer(masks, Access::Clear);
            let k = b.write_buffer(counters, Access::Clear);
            ((m, k), move |ctx: &mut PassContext<'_>| {
                ctx.fill_buffer(m, 0);
                ctx.fill_buffer(k, 0);
            })
        });

        let pipeline = &self.bounds;
        let bounds = g.add_compute("UiBounds", |b| {
            let out = b.write_buffer(bounds, Access::ComputeWrite);
            (out, move |ctx: &mut PassContext<'_>| {
                let args = BoundsArgs {
                    frame: ctx.upload(&frame),
                    commands: ctx.upload_slice(cmds),
                    payload: ctx.upload_slice(payload),
                    glyphs: ctx.upload_slice(glyphs),
                    bounds: ctx.uav(out),
                };
                ctx.set_pipeline(pipeline);
                ctx.push(&args);
                ctx.dispatch(commands.div_ceil(256).max(1), 1, 1);
            })
        });

        let pipeline = &self.bin;
        let masks = g.add_compute("UiBin", |b| {
            let bd = b.read_buffer(bounds, Access::ComputeRead);
            let m = b.write_buffer(masks, Access::ComputeWrite);
            (m, move |ctx: &mut PassContext<'_>| {
                let args = BinArgs {
                    frame: ctx.upload(&frame),
                    bounds: ctx.srv(bd),
                    masks: ctx.uav(m),
                };
                ctx.set_pipeline(pipeline);
                ctx.push(&args);
                ctx.dispatch(commands.div_ceil(256).max(1), 1, 1);
            })
        });

        let pipeline = &self.coarse;
        let (heads, pool, counters) = g.add_compute("UiCoarse", |b| {
            let bd = b.read_buffer(bounds, Access::ComputeRead);
            let m = b.read_buffer(masks, Access::ComputeRead);
            let h = b.write_buffer(heads, Access::ComputeWrite);
            let p = b.write_buffer(pool, Access::ComputeWrite);
            let k = b.write_buffer(counters, Access::ComputeWrite);
            ((h, p, k), move |ctx: &mut PassContext<'_>| {
                let args = CoarseArgs {
                    frame: ctx.upload(&frame),
                    commands: ctx.upload_slice(cmds),
                    payload: ctx.upload_slice(payload),
                    bounds: ctx.srv(bd),
                    masks: ctx.srv(m),
                    heads: ctx.uav(h),
                    pool: ctx.uav(p),
                    counters: ctx.uav(k),
                };
                ctx.set_pipeline(pipeline);
                ctx.push(&args);
                ctx.dispatch(bins_x, bins_y, 1);
            })
        });

        // The pool's use, for sizing the next frames.
        let slot = (g.frame() % FRAMES_IN_FLIGHT as u64) as usize;
        let readback = &self.readbacks[slot];
        let rb = g.import_buffer("UiCountersReadback", readback);
        let pool_size = &self.pool_blocks;
        let overflowed = &self.overflowed;
        g.add_copy("UiCounters", |b| {
            b.side_effect();
            let k = b.read_buffer(counters, Access::CopySrc);
            let r = b.write_buffer(rb, Access::CopyDst);
            ((), move |ctx: &mut PassContext<'_>| {
                // The slot's last frame has finished: its count is in.
                let mut bytes = [0u8; 8];
                // SAFETY: the frame that last wrote this buffer has finished (its frame slot
                // was waited for before recording), and no other frame writes it.
                unsafe { readback.read(0, &mut bytes) };
                let used = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let over = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                let current = pool_size.load(Ordering::Relaxed);
                let want = if over != 0 {
                    overflowed.store(true, Ordering::Relaxed);
                    current.saturating_mul(2)
                } else {
                    // Headroom over the last use.
                    used.saturating_add(used / 2)
                };
                if want > current {
                    pool_size.store(want, Ordering::Relaxed);
                }
                ctx.copy_buffer(k, 0, r, 0, 8);
            })
        });

        let target = g.create_texture("UiTarget", TextureDesc::new_2d(width, height, UI_FORMAT));
        let pipeline = &self.fine;
        let target = g.add_compute("UiFine", |b| {
            let h = b.read_buffer(heads, Access::ComputeRead);
            let p = b.read_buffer(pool, Access::ComputeRead);
            let gp = b.read_texture(pages, Access::ComputeRead);
            let out = b.write_texture(target, Access::ComputeWrite);
            (out, move |ctx: &mut PassContext<'_>| {
                let args = FineArgs {
                    frame: ctx.upload(&frame),
                    commands: ctx.upload_slice(cmds),
                    payload: ctx.upload_slice(payload),
                    paints: ctx.upload_slice(paints),
                    glyphs: ctx.upload_slice(glyphs),
                    heads: ctx.srv(h),
                    pool: ctx.srv(p),
                    glyph_pages: ctx.srv(gp),
                    output: ctx.uav(out),
                };
                ctx.set_pipeline(pipeline);
                ctx.push(&args);
                ctx.dispatch(tiles_x, tiles_y, 1);
            })
        });
        UiOutput {
            target,
            heads,
            pool,
        }
    }

    fn upload_glyphs<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        uploads: &'g [GlyphUpload],
        pages: TextureHandle<'g>,
    ) -> TextureHandle<'g> {
        let pipeline = &self.glyph_upload;
        let (mut max_w, mut max_h) = (0, 0);
        for u in uploads {
            max_w = max_w.max(u.rect.width);
            max_h = max_h.max(u.rect.height);
        }
        g.add_compute("UiGlyphUpload", |b| {
            let out = b.write_texture(pages, Access::ComputeWrite);
            (out, move |ctx: &mut PassContext<'_>| {
                let mut records = Vec::with_capacity(uploads.len());
                let mut data: Vec<u32> = Vec::new();
                for u in uploads {
                    records.push([
                        u.rect.x | u.rect.y << 16,
                        u.rect.width | u.rect.height << 16,
                        u.page,
                        data.len() as u32,
                    ]);
                    for word in u.coverage.chunks(4) {
                        let mut w = [0u8; 4];
                        w[..word.len()].copy_from_slice(word);
                        data.push(u32::from_le_bytes(w));
                    }
                }
                let args = GlyphUploadArgs {
                    uploads: ctx.upload_slice(&records),
                    data: ctx.upload_slice(&data),
                    pages: ctx.uav(out),
                };
                ctx.set_pipeline(pipeline);
                ctx.push(&args);
                ctx.dispatch(max_w.div_ceil(8), max_h.div_ceil(8), uploads.len() as u32);
            })
        })
    }

    /// Writes the UI target `ui` into `target` (the format the renderer was made for), and
    /// returns it.
    pub fn encode<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        ui: TextureHandle<'g>,
        target: TextureHandle<'g>,
    ) -> TextureHandle<'g> {
        let (pipeline, decode) = self
            .encode
            .as_ref()
            .expect("a renderer made with an output format");
        let decode = u32::from(*decode);
        g.add_raster("UiEncode", |b| {
            let out = b.color(target, LoadOp::DontCare, StoreOp::Store);
            let src = b.read_texture(ui, Access::PixelRead);
            (out, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(pipeline);
                ctx.push(&EncodeArgs {
                    ui: ctx.srv(src),
                    decode,
                });
                ctx.draw(3, 1, 0, 0);
            })
        })
    }

    /// Makes `frames` forget the renderer's own resources; call before dropping it.
    pub fn forget(&self, frames: &mut FrameContext) {
        frames.forget_texture(&self.pages);
        for b in &self.readbacks {
            frames.forget_buffer(b);
        }
    }
}
