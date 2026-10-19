//! The GPU side (0012 §5): the passes, on the render graph.
//!
//! A [`UiRenderer`] holds what every surface shares: the pipelines, the sampler, the glyph cache
//! and its pages. A [`UiSurface`] is one window or render target: its persistent UI target and
//! tile hashes (damage detection, §5.5), and its tile list pool.
//!
//! A frame:
//!
//! 1. [`UiRenderer::prepare`] (CPU): every surface's draw list flattened, their glyphs made
//!    resident at once (new ones rasterized), every command hashed.
//! 2. [`UiSurface::needs_redraw`]: whether a surface's frame draws anything new; when not, the
//!    caller declares nothing for it and does not present it (§5.5's whole-frame skip).
//! 3. [`UiRenderer::begin`], once: the new glyphs into the pages.
//! 4. [`UiRenderer::add_passes`] per surface: bounds, binning, paths' tiles, damage and shading
//!    into its UI target, with the frame's external images (viewports, §7) read where they are
//!    drawn.
//! 5. [`UiRenderer::encode`]: a UI target into a window's image.
//!
//! The tile lists come from a pool of blocks sized from what earlier frames used: each frame
//! copies its count into a small readback buffer, read when its frame slot comes round again.
//! A frame that runs out of blocks drops commands from the tiles it could not extend (and says
//! so through [`UiSurface::pool_overflowed`]); the next frames get twice the pool.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

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
    Result, Sampler, SamplerDesc, ShaderModule, ShaderView, StoreOp, Texture, TextureUsage,
    TextureViewDesc,
};
use marlin_serial::cooked::Pod;
use marlin_shader::asset::ShaderLibrary;
use marlin_shader::bind::{BindError, EntryDesc, bind};
use marlin_shader::reflect::Stage;

use crate::blur::{self, BlurJob};
use crate::command::{
    BIN, BLOCK_WORDS, CHUNK_COMMANDS, Chunk, DEFAULT_SAMPLER, Kind, PAINT_BACKDROP, PAINT_EXTERNAL,
    PAINT_IMAGE, TILE,
};
use crate::damage::{command_hashes, frame_hash};
use crate::list::UiDrawList;
use crate::path::PATH_TILE_WORDS;
use crate::style::{Color, ExternalImageId};

/// The most tile list blocks the pool grows to (256 MiB).
pub const MAX_POOL_BLOCKS: u32 = 1 << 21;

/// The workgroups UiCoarse walks the bins with at most: a bin crowded with commands is cut into
/// segments walked by a workgroup each, up to this over the bins and to 64 a bin.
const COARSE_WORKGROUPS: u32 = 2048;

/// A bin's work in UiCoarse: its commands, and `CHUNK_WORK` for each chunk whose masks it walks.
/// A crowded bin is cut into a segment per `SEGMENT_WORK` of it (`SegmentWork`, `ChunkWork`).
const SEGMENT_WORK: u32 = 1024;
const CHUNK_WORK: u32 = 4;

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
    pub blur: ShaderModule<'a>,
    pub path_tiles: ShaderModule<'a>,
    pub coarse_merge: ShaderModule<'a>,
    pub coarse_copy: ShaderModule<'a>,
    pub coarse_plan: ShaderModule<'a>,
}

impl<'a> UiShaders<'a> {
    /// The entries, in field order.
    pub const ENTRIES: [EntryDesc; 12] = [
        EntryDesc::compute("Marlin.Engine.Ui.GlyphUpload").push::<GlyphUploadArgs>(),
        EntryDesc::compute("Marlin.Engine.Ui.Bounds").push::<BoundsArgs>(),
        EntryDesc::compute("Marlin.Engine.Ui.Bin").push::<BinArgs>(),
        EntryDesc::compute("Marlin.Engine.Ui.Coarse").push::<CoarseArgs>(),
        EntryDesc::compute("Marlin.Engine.Ui.Fine").push::<FineArgs>(),
        EntryDesc::new("Marlin.Engine.Ui.EncodeVs", Stage::Vertex),
        EntryDesc::new("Marlin.Engine.Ui.EncodePs", Stage::Pixel).push::<EncodeArgs>(),
        EntryDesc::compute("Marlin.Engine.Ui.Blur").push::<BlurArgs>(),
        EntryDesc::compute("Marlin.Engine.Ui.PathTiles").push::<PathTilesArgs>(),
        EntryDesc::compute("Marlin.Engine.Ui.CoarseMerge").push::<CoarseArgs>(),
        EntryDesc::compute("Marlin.Engine.Ui.CoarseCopy").push::<CoarseArgs>(),
        EntryDesc::compute("Marlin.Engine.Ui.CoarsePlan").push::<CoarsePlanArgs>(),
    ];

    /// The shaders from `modules`, in [`ENTRIES`](Self::ENTRIES) order.
    pub fn from_modules(
        [
            glyph_upload,
            bounds,
            bin,
            coarse,
            fine,
            encode_vs,
            encode_ps,
            blur,
            path_tiles,
            coarse_merge,
            coarse_copy,
            coarse_plan,
        ]: [ShaderModule<'a>; 12],
    ) -> Self {
        Self {
            glyph_upload,
            bounds,
            bin,
            coarse,
            fine,
            encode_vs,
            encode_ps,
            blur,
            path_tiles,
            coarse_merge,
            coarse_copy,
            coarse_plan,
        }
    }

    /// The UI renderer's shaders from `library`, checked against what it sets.
    pub fn bind(library: &'a ShaderLibrary) -> core::result::Result<Self, BindError> {
        bind(library, &Self::ENTRIES).map(Self::from_modules)
    }
}

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
    /// Tile hashes start from it: what changes every tile.
    salt: u32,
    /// Nonzero: every tile is dirty (the target is new, or damage detection is off).
    force: u32,
    /// Layers the frame is drawn in (backdrop blurs, §5.6).
    layers: u32,
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
    groups: u32,
    clips: u32,
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
struct CoarsePlanArgs {
    frame: u32,
    masks: u32,
    plans: u32,
    counters: u32,
    max_segments: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct CoarseArgs {
    frame: u32,
    commands: u32,
    payload: u32,
    groups: u32,
    hashes: u32,
    bounds: u32,
    masks: u32,
    heads: u32,
    pool: u32,
    counters: u32,
    tile_hashes: u32,
    layer_starts: u32,
    plans: u32,
    lists: u32,
    max_segments: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct FineArgs {
    frame: u32,
    commands: u32,
    payload: u32,
    paints: u32,
    gradients: u32,
    glyphs: u32,
    groups: u32,
    clips: u32,
    heads: u32,
    pool: u32,
    glyph_pages: u32,
    output: u32,
    first: u32,
    end: u32,
    previous: u32,
    backdrops: u32,
    paths: u32,
    path_tiles: u32,
    path_entries: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct PathTilesArgs {
    payload: u32,
    paths: u32,
    items: u32,
    tiles: u32,
    counter: u32,
    entries: u32,
    entry_capacity: u32,
    mode: u32,
    count: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct BlurArgs {
    source: u32,
    output: u32,
    mode: u32,
    scale: u32,
    origin: u32,
    size: u32,
    source_size: u32,
    sigma: f32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct EncodeArgs {
    ui: u32,
    decode: u32,
}

/// Workgroups a dispatch's dimension takes at most.
const MAX_GROUPS: u32 = 65535;

/// A surface's paths in this frame's graph: their records, and when any has tiles on the
/// surface, the tiles and their entries.
struct PathTiles<'g> {
    records: Vec<u32>,
    buffers: Option<(BufferHandle<'g>, BufferHandle<'g>)>,
}

/// How a [`UiRenderer`] is set up.
#[derive(Clone, Copy, Debug)]
pub struct UiRendererDesc {
    pub glyph_cache: GlyphCacheConfig,
    /// The format [`UiRenderer::encode`] writes (the swapchain's); `None` for a renderer that
    /// only draws UI targets.
    pub output_format: Option<Format>,
    /// Shade only the tiles whose content changed (§5.5). Off, every tile is shaded every
    /// frame (tests compare the two).
    pub damage: bool,
}

impl Default for UiRendererDesc {
    fn default() -> Self {
        Self {
            glyph_cache: GlyphCacheConfig::default(),
            output_format: None,
            damage: true,
        }
    }
}

/// One surface's part of a frame, ready for [`UiRenderer::add_passes`].
pub struct UiFrameData {
    chunk: Chunk,
    hashes: Vec<u32>,
}

impl UiFrameData {
    /// Every command of the frame, as the GPU gets them.
    pub fn commands(&self) -> &Chunk {
        &self.chunk
    }

    /// Every command's content hash.
    pub fn hashes(&self) -> &[u32] {
        &self.hashes
    }
}

/// A frame's CPU work for every surface.
pub struct UiPrepared {
    pub surfaces: Vec<UiFrameData>,
    glyphs: Vec<GlyphRecord>,
    uploads: Vec<GlyphUpload>,
    pub stats: PrepareStats,
}

impl UiPrepared {
    /// The glyph table the frame draws with.
    pub fn glyph_table(&self) -> &[GlyphRecord] {
        &self.glyphs
    }

    /// The glyph bitmaps the frame adds to the cache's pages.
    pub fn uploads(&self) -> &[GlyphUpload] {
        &self.uploads
    }
}

/// The glyph pages in this frame's graph, after [`UiRenderer::begin`].
#[derive(Clone, Copy, Debug)]
pub struct UiPages<'g> {
    pages: TextureHandle<'g>,
    glyphs: &'g [[u32; 4]],
}

/// What [`UiRenderer::add_passes`] declared.
#[derive(Clone, Copy, Debug)]
pub struct UiOutput<'g> {
    /// The UI target: [`UI_FORMAT`], sRGB-encoded, premultiplied; the surface's, kept across
    /// frames (a texture surface's texture, every mip).
    pub target: TextureHandle<'g>,
    /// Per tile (row by row): its list's first block, its length, whether it was shaded (four
    /// words).
    pub heads: BufferHandle<'g>,
    /// The blocks: [`BLOCK_WORDS`] words each, the last one the next block.
    pub pool: BufferHandle<'g>,
    /// When a path has tiles on the surface: every path tile ([`crate::path::PATH_TILE_WORDS`]
    /// words, at its record's first tile, row by row), and their entries (§4.2).
    pub paths: Option<(BufferHandle<'g>, BufferHandle<'g>)>,
}

/// How a [`UiSurface`] is set up.
#[derive(Clone, Copy, Debug)]
pub struct UiSurfaceDesc {
    /// Keys its persistent resources in the graph: unique among the surfaces.
    pub key: u64,
    /// Tile list blocks the first frames get (128 bytes each).
    pub initial_pool_blocks: u32,
}

impl UiSurfaceDesc {
    pub fn new(key: u64) -> Self {
        Self {
            key,
            initial_pool_blocks: 16384,
        }
    }
}

/// A surface drawn into a texture the caller samples (§10: UI in the world, on a material)
/// rather than a window.
#[derive(Clone, Copy, Debug)]
pub struct UiTextureDesc {
    pub width: u32,
    pub height: u32,
    /// A full mip chain, remade after every redraw (each mip the 2 × 2 average of the one
    /// above, in the UI target's encoding).
    pub mips: bool,
}

/// A texture surface's own texture.
struct SurfaceTexture {
    texture: Texture,
    view: ShaderView,
    width: u32,
    height: u32,
    mips: u32,
    /// Whether a frame has drawn it: until then every tile is shaded.
    drawn: AtomicBool,
}

/// One window or render target the UI is drawn to.
pub struct UiSurface {
    key: u64,
    /// The pool's size this frame, raised from what frames used.
    pool_blocks: AtomicU32,
    overflowed: AtomicBool,
    /// The most work a bin had in the last frame read back (`u32::MAX` before one is): how
    /// far UiCoarse cuts bins.
    bin_work: AtomicU32,
    /// Per frame slot: the counters' copy, and whether a frame has written it yet.
    readbacks: Vec<(Buffer, AtomicBool)>,
    /// The last frame drawn: its hash (0: none yet).
    last: AtomicU64,
    texture: Option<SurfaceTexture>,
}

impl UiSurface {
    pub fn new(device: &Device, desc: &UiSurfaceDesc) -> Result<Self> {
        let readbacks = (0..FRAMES_IN_FLIGHT)
            .map(|_| {
                device
                    .create_buffer(&marlin_rhi::BufferDesc {
                        size: 12,
                        usage: BufferUsage::COPY_DST,
                        memory: MemoryLocation::Readback,
                    })
                    .map(|b| (b, AtomicBool::new(false)))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            key: desc.key,
            pool_blocks: AtomicU32::new(desc.initial_pool_blocks.max(64)),
            overflowed: AtomicBool::new(false),
            bin_work: AtomicU32::new(u32::MAX),
            readbacks,
            last: AtomicU64::new(0),
            texture: None,
        })
    }

    /// A surface drawn into its own texture (§10): [`UI_FORMAT`], sampled through an sRGB view
    /// and written by the UI's passes. The view's bindless index ([`view`](Self::view)) stays
    /// the same for the surface's life, so a material can hold it.
    pub fn new_texture(device: &Device, desc: &UiSurfaceDesc, t: &UiTextureDesc) -> Result<Self> {
        let mips = if t.mips {
            32 - t.width.max(t.height).max(1).leading_zeros()
        } else {
            1
        };
        let texture = device.create_texture(&marlin_rhi::TextureDesc {
            mip_levels: mips,
            ..marlin_rhi::TextureDesc::new_2d(
                t.width,
                t.height,
                UI_FORMAT,
                TextureUsage::SAMPLED | TextureUsage::STORAGE | TextureUsage::COPY_SRC,
            )
        })?;
        let view = device.create_texture_view(
            &texture,
            &TextureViewDesc {
                format: Some(Format::Rgba8UnormSrgb),
                ..TextureViewDesc::SAMPLED
            },
        )?;
        Ok(Self {
            texture: Some(SurfaceTexture {
                texture,
                view,
                width: t.width,
                height: t.height,
                mips,
                drawn: AtomicBool::new(false),
            }),
            ..Self::new(device, desc)?
        })
    }

    /// A texture surface's texture: every mip, premultiplied. Through [`view`](Self::view) a
    /// texel reads as linear color where the UI is opaque; where it is translucent, its color
    /// was premultiplied before the sRGB encoding (§8.1) and reads a little dark.
    pub fn texture(&self) -> Option<&Texture> {
        self.texture.as_ref().map(|t| &t.texture)
    }

    /// A texture surface's sampled view (sRGB, every mip).
    pub fn view(&self) -> Option<&ShaderView> {
        self.texture.as_ref().map(|t| &t.view)
    }

    /// A texture surface's size.
    pub fn size(&self) -> Option<(u32, u32)> {
        self.texture.as_ref().map(|t| (t.width, t.height))
    }

    /// Whether `data` at `width` × `height` over `clear` draws anything this surface has not
    /// drawn last time; it is then remembered as drawn. A surface that does not need a redraw
    /// keeps its pixels: declare nothing for it and do not present it (§5.5).
    pub fn needs_redraw(&self, data: &UiFrameData, width: u32, height: u32, clear: Color) -> bool {
        let h = frame_hash(&data.hashes, width, height, clear.packed()).max(1);
        self.last.swap(h, Ordering::Relaxed) != h
    }

    /// Forgets the last frame: the next one redraws.
    pub fn invalidate(&self) {
        self.last.store(0, Ordering::Relaxed);
    }

    /// Whether a frame ran out of tile list blocks since the last call.
    pub fn pool_overflowed(&self) -> bool {
        self.overflowed.swap(false, Ordering::Relaxed)
    }

    /// Tile list blocks the next frame gets.
    pub fn pool_blocks(&self) -> u32 {
        self.pool_blocks.load(Ordering::Relaxed)
    }

    /// Makes `frames` forget the surface's own resources; call before dropping it.
    pub fn forget(&self, frames: &mut FrameContext) {
        for (b, _) in &self.readbacks {
            frames.forget_buffer(b);
        }
        if let Some(t) = &self.texture {
            frames.forget_texture(&t.texture);
        }
    }
}

/// What every surface shares: pipelines, the glyph cache and its pages.
pub struct UiRenderer {
    glyph_upload: ComputePipeline,
    bounds: ComputePipeline,
    bin: ComputePipeline,
    coarse: ComputePipeline,
    coarse_merge: ComputePipeline,
    coarse_copy: ComputePipeline,
    coarse_plan: ComputePipeline,
    fine: ComputePipeline,
    blur: ComputePipeline,
    path_tiles: ComputePipeline,
    encode: Option<(GraphicsPipeline, bool)>,
    sampler: Sampler,
    glyphs: GlyphCache,
    pages: Texture,
    damage: bool,
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
        Ok(Self {
            glyph_upload: compute(shaders.glyph_upload)?,
            bounds: compute(shaders.bounds)?,
            bin: compute(shaders.bin)?,
            coarse: compute(shaders.coarse)?,
            coarse_merge: compute(shaders.coarse_merge)?,
            coarse_copy: compute(shaders.coarse_copy)?,
            coarse_plan: compute(shaders.coarse_plan)?,
            fine: compute(shaders.fine)?,
            blur: compute(shaders.blur)?,
            path_tiles: compute(shaders.path_tiles)?,
            encode,
            sampler: device.create_sampler(&SamplerDesc {
                address_u: AddressMode::ClampToEdge,
                address_v: AddressMode::ClampToEdge,
                address_w: AddressMode::ClampToEdge,
                ..SamplerDesc::default()
            })?,
            glyphs: GlyphCache::new(desc.glyph_cache),
            pages,
            damage: desc.damage,
        })
    }

    /// The glyph cache, whose ids painters write ([`UiDrawList::painter`]).
    pub fn glyph_cache(&self) -> &GlyphCache {
        &self.glyphs
    }

    /// The CPU part of a frame for every surface's list: their glyphs made resident at once
    /// (`fonts[i]` is `FontId(i)`), rasterizing new ones on `jobs`, their commands flattened and
    /// hashed. `externals` give the version of each external image the lists draw: a new
    /// version redraws where the image is.
    pub fn prepare(
        &mut self,
        lists: &[&UiDrawList],
        externals: &[(ExternalImageId, u64)],
        fonts: &[FontView<'_>],
        jobs: Option<&JobSystem>,
    ) -> UiPrepared {
        let sampler = self.sampler.index();
        let mut chunks: Vec<Chunk> = lists.iter().map(|l| l.flatten()).collect();
        let mut used = Vec::new();
        for chunk in &mut chunks {
            for p in &mut chunk.paints {
                if matches!(p[0], PAINT_IMAGE | PAINT_EXTERNAL | PAINT_BACKDROP)
                    && p[3] == DEFAULT_SAMPLER
                {
                    p[3] = sampler;
                }
            }
            // Backdrop blurs: their index among the frame's, in the payload and the paint.
            let mut blurs = 0;
            for c in &chunk.commands {
                if Kind::from_word(c[0]) == Some(Kind::Blur) {
                    chunk.payload[c[3] as usize + 9] = blurs;
                    chunk.paints[c[2] as usize][2] = blurs;
                    blurs += 1;
                }
            }
            used.extend(
                chunk
                    .commands
                    .iter()
                    .filter(|c| Kind::from_word(c[0]) == Some(Kind::Glyph))
                    .map(|c| marlin_font::GlyphId(chunk.payload[c[3] as usize + 2])),
            );
        }
        let stats = self.glyphs.prepare(fonts, &used, jobs);
        let glyphs = self.glyphs.table();
        let surfaces = chunks
            .into_iter()
            .map(|mut chunk| {
                let hashes = command_hashes(&chunk, &glyphs, externals);
                // Paths: their index among the frame's, for their records (the hashes leave
                // it out).
                let mut paths = 0;
                for c in &chunk.commands {
                    if Kind::from_word(c[0]) == Some(Kind::Path) {
                        chunk.payload[c[3] as usize + 4] = paths;
                        paths += 1;
                    }
                }
                UiFrameData { hashes, chunk }
            })
            .collect();
        UiPrepared {
            surfaces,
            glyphs,
            uploads: self.glyphs.take_uploads(),
            stats,
        }
    }

    /// Declares the frame's glyph uploads; once a frame, before any surface's passes.
    pub fn begin<'g>(&'g self, g: &mut RenderGraph<'g>, prepared: &'g UiPrepared) -> UiPages<'g> {
        let pages = g.import_texture("UiGlyphPages", &self.pages);
        let pages = if prepared.uploads.is_empty() {
            pages
        } else {
            self.upload_glyphs(g, &prepared.uploads, pages)
        };
        let glyphs: &'g [[u32; 4]] = if prepared.glyphs.is_empty() {
            &[[0; 4]]
        } else {
            &prepared.glyphs
        };
        UiPages { pages, glyphs }
    }

    /// Declares `surface`'s passes for `data` on a `width` × `height` target over `clear`, and
    /// returns its UI target. `externals` give the textures of the external images the list
    /// draws (one that is missing draws nothing). Needs a graph with a state table (the frame
    /// context's).
    #[allow(clippy::too_many_arguments)]
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        pages: UiPages<'g>,
        surface: &'g UiSurface,
        data: &'g UiFrameData,
        width: u32,
        height: u32,
        clear: Color,
        externals: &[(ExternalImageId, TextureHandle<'g>)],
    ) -> UiOutput<'g> {
        let commands = data.chunk.commands.len() as u32;
        let chunks = commands.div_ceil(CHUNK_COMMANDS);
        let (bins_x, bins_y) = (width.div_ceil(BIN), height.div_ceil(BIN));
        let (tiles_x, tiles_y) = (width.div_ceil(TILE), height.div_ceil(TILE));
        let tiles = u64::from(tiles_x * tiles_y);
        let pool_blocks = surface.pool_blocks.load(Ordering::Relaxed);
        let key = surface.key;

        // The target and the tile hashes outlive the frame (§5.5): new ones redraw everything.
        // A texture surface draws into its own texture (§10).
        let (mut target, fresh) = match &surface.texture {
            Some(t) => {
                assert_eq!(
                    (width, height),
                    (t.width, t.height),
                    "a texture surface is drawn at its texture's size"
                );
                (
                    g.import_texture("UiSurfaceTexture", &t.texture),
                    !t.drawn.swap(true, Ordering::Relaxed),
                )
            }
            None => {
                let t = g.persistent(
                    "UiTarget",
                    key,
                    TextureDesc::new_2d(width, height, UI_FORMAT),
                );
                (t.handle, t.fresh)
            }
        };
        let tile_hashes = g.persistent("UiTileHashes", key, BufferDesc::structured::<u32>(tiles));
        // Backdrop blurs draw the frame in layers (§5.6). What a blur shows depends on pixels
        // outside its tiles, so a frame with blurs shades every tile.
        let plan = blur::plan(&data.chunk, pages.glyphs, width, height);
        let layers = plan.starts.len();
        let force = !self.damage || fresh || tile_hashes.fresh || layers > 1;
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
            salt: (frame_hash(&[], width, height, clear.packed()) as u32).max(1),
            force: u32::from(force),
            layers: layers as u32,
            clear: crate::shape::premultiplied(clear.packed()),
        };
        // Uploads need at least one element.
        let one = |s: &'g [[u32; 4]]| if s.is_empty() { &[[0u32; 4]][..] } else { s };
        let ones = |s: &'g [u32]| if s.is_empty() { &[0u32][..] } else { s };
        let cmds: &'g [[u32; 4]] = one(&data.chunk.commands);
        let paints: &'g [[u32; 4]] = one(&data.chunk.paints);
        let payload: &'g [u32] = ones(&data.chunk.payload);
        let hashes: &'g [u32] = ones(&data.hashes);
        // Never empty: group 0 and clip 0 are always there.
        let groups: &'g [u32] = data.chunk.groups.as_flattened();
        let clips: &'g [u32] = data.chunk.clips.as_flattened();
        let gradients: &'g [u32] = ones(data.chunk.gradients.as_flattened());
        let glyphs = pages.glyphs;

        let bounds = g.create_buffer(
            "UiBounds",
            BufferDesc::structured::<[u32; 4]>(u64::from(commands.max(1))),
        );
        let masks = g.create_buffer(
            "UiMasks",
            BufferDesc::structured::<u32>(u64::from(chunks.max(1) * bins_x * bins_y * 8)),
        );
        let heads = g.create_buffer("UiTileHeads", BufferDesc::structured::<[u32; 4]>(tiles));
        let pool = g.create_buffer(
            "UiTilePool",
            BufferDesc::structured::<u32>(u64::from(pool_blocks * BLOCK_WORDS)),
        );
        let counters = g.create_buffer("UiCounters", BufferDesc::structured::<u32>(3));

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
                    groups: ctx.upload_slice(groups),
                    clips: ctx.upload_slice(clips),
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

        // Coarse binning: each bin walked by one workgroup, or a crowded one cut into segments
        // walked by a workgroup each, their parts of the tile lists joined after. How far bins
        // are cut follows the most crowded bin of a recent frame (all the way until one is read
        // back): a frame whose bins were all small skips the planning and the joining.
        let crowd = surface
            .bin_work
            .load(Ordering::Relaxed)
            .min(commands + CHUNK_WORK * chunks);
        let max_segments = if crowd > SEGMENT_WORK {
            crowd
                .div_ceil(SEGMENT_WORK)
                .min((COARSE_WORKGROUPS / (bins_x * bins_y)).clamp(1, 64))
        } else {
            1
        };
        // What a pass leaves out of its arguments is 0.
        let base = CoarseArgs {
            frame: 0,
            commands: 0,
            payload: 0,
            groups: 0,
            hashes: 0,
            bounds: 0,
            masks: 0,
            heads: 0,
            pool: 0,
            counters: 0,
            tile_hashes: 0,
            layer_starts: 0,
            plans: 0,
            lists: 0,
            max_segments,
        };
        let split = (max_segments > 1).then(|| {
            let plans = g.create_buffer(
                "UiCoarsePlans",
                BufferDesc::structured::<u32>(u64::from(bins_x * bins_y * (max_segments + 2))),
            );
            let lists = g.create_buffer(
                "UiCoarseLists",
                BufferDesc::structured::<u32>(4 * tiles * u64::from(max_segments)),
            );
            let pipeline = &self.coarse_plan;
            let (plans, counters) = g.add_compute("UiCoarsePlan", |b| {
                let m = b.read_buffer(masks, Access::ComputeRead);
                let pl = b.write_buffer(plans, Access::ComputeWrite);
                let k = b.write_buffer(counters, Access::ComputeWrite);
                ((pl, k), move |ctx: &mut PassContext<'_>| {
                    let args = CoarsePlanArgs {
                        frame: ctx.upload(&frame),
                        masks: ctx.srv(m),
                        plans: ctx.uav(pl),
                        counters: ctx.uav(k),
                        max_segments,
                    };
                    ctx.set_pipeline(pipeline);
                    ctx.push(&args);
                    ctx.dispatch(bins_x, bins_y, 1);
                })
            });
            (plans, lists, counters)
        });
        let (split, counters) = match split {
            Some((plans, lists, counters)) => (Some((plans, lists)), counters),
            None => (None, counters),
        };

        let starts = plan.starts.clone();
        let pipeline = &self.coarse;
        let (split, heads, pool, counters, tile_hashes) = g.add_compute("UiCoarse", |b| {
            let bd = b.read_buffer(bounds, Access::ComputeRead);
            let m = b.read_buffer(masks, Access::ComputeRead);
            let split = split.map(|(pl, l)| {
                (
                    b.read_buffer(pl, Access::ComputeRead),
                    b.write_buffer(l, Access::ComputeWrite),
                )
            });
            let h = b.write_buffer(heads, Access::ComputeWrite);
            let p = b.write_buffer(pool, Access::ComputeWrite);
            let k = b.write_buffer(counters, Access::ComputeWrite);
            let th = b.write_buffer(tile_hashes.handle, Access::ComputeWrite);
            ((split, h, p, k, th), move |ctx: &mut PassContext<'_>| {
                let (plans, lists) = split.map_or((0, 0), |(pl, l)| (ctx.srv(pl), ctx.uav(l)));
                let args = CoarseArgs {
                    frame: ctx.upload(&frame),
                    commands: ctx.upload_slice(cmds),
                    payload: ctx.upload_slice(payload),
                    groups: ctx.upload_slice(groups),
                    hashes: ctx.upload_slice(hashes),
                    bounds: ctx.srv(bd),
                    masks: ctx.srv(m),
                    heads: ctx.uav(h),
                    pool: ctx.uav(p),
                    counters: ctx.uav(k),
                    tile_hashes: ctx.uav(th),
                    layer_starts: ctx.upload_slice(&starts),
                    plans,
                    lists,
                    ..base
                };
                ctx.set_pipeline(pipeline);
                ctx.push(&args);
                ctx.dispatch(bins_x, bins_y, max_segments);
            })
        });

        let (heads, pool, counters) = match split {
            None => (heads, pool, counters),
            Some((plans, lists)) => {
                let pipeline = &self.coarse_merge;
                let (lists, heads, pool, counters) = g.add_compute("UiCoarseMerge", |b| {
                    let pl = b.read_buffer(plans, Access::ComputeRead);
                    let l = b.write_buffer(lists, Access::ComputeWrite);
                    let h = b.write_buffer(heads, Access::ComputeWrite);
                    let p = b.write_buffer(pool, Access::ComputeWrite);
                    let k = b.write_buffer(counters, Access::ComputeWrite);
                    let th = b.write_buffer(tile_hashes, Access::ComputeWrite);
                    ((l, h, p, k), move |ctx: &mut PassContext<'_>| {
                        let args = CoarseArgs {
                            frame: ctx.upload(&frame),
                            hashes: ctx.upload_slice(hashes),
                            heads: ctx.uav(h),
                            pool: ctx.uav(p),
                            counters: ctx.uav(k),
                            tile_hashes: ctx.uav(th),
                            plans: ctx.srv(pl),
                            lists: ctx.uav(l),
                            ..base
                        };
                        ctx.set_pipeline(pipeline);
                        ctx.push(&args);
                        ctx.dispatch(bins_x, bins_y, 1);
                    })
                });
                let pipeline = &self.coarse_copy;
                let (heads, pool) = g.add_compute("UiCoarseCopy", |b| {
                    let pl = b.read_buffer(plans, Access::ComputeRead);
                    let l = b.write_buffer(lists, Access::ComputeWrite);
                    let h = b.write_buffer(heads, Access::ComputeWrite);
                    let p = b.write_buffer(pool, Access::ComputeWrite);
                    ((h, p), move |ctx: &mut PassContext<'_>| {
                        let args = CoarseArgs {
                            frame: ctx.upload(&frame),
                            heads: ctx.uav(h),
                            pool: ctx.uav(p),
                            plans: ctx.srv(pl),
                            lists: ctx.uav(l),
                            ..base
                        };
                        ctx.set_pipeline(pipeline);
                        ctx.push(&args);
                        ctx.dispatch(bins_x, bins_y, max_segments);
                    })
                });
                (heads, pool, counters)
            }
        };

        // The pool's use, for sizing the next frames.
        let slot = (g.frame() % FRAMES_IN_FLIGHT as u64) as usize;
        let (readback, written) = &surface.readbacks[slot];
        let rb = g.import_buffer("UiCountersReadback", readback);
        let pool_size = &surface.pool_blocks;
        let crowd = &surface.bin_work;
        let overflowed = &surface.overflowed;
        g.add_copy("UiCounters", |b| {
            b.side_effect();
            let k = b.read_buffer(counters, Access::CopySrc);
            let r = b.write_buffer(rb, Access::CopyDst);
            ((), move |ctx: &mut PassContext<'_>| {
                // The slot's last frame has finished: its count is in (once there was one).
                let mut bytes = [0u8; 12];
                if written.swap(true, Ordering::Relaxed) {
                    // SAFETY: the frame that last wrote this buffer has finished (its frame
                    // slot was waited for before recording), and no other frame writes it.
                    unsafe { readback.read(0, &mut bytes) };
                }
                let used = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let over = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                crowd.store(
                    u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
                    Ordering::Relaxed,
                );
                let current = pool_size.load(Ordering::Relaxed);
                let want = if over != 0 {
                    overflowed.store(true, Ordering::Relaxed);
                    current.saturating_mul(2)
                } else {
                    // Headroom over the last use.
                    used.saturating_add(used / 2)
                };
                if want > current {
                    pool_size.store(want.min(MAX_POOL_BLOCKS), Ordering::Relaxed);
                }
                ctx.copy_buffer(k, 0, r, 0, 12);
            })
        });

        let path_tiles = self.add_path_tiles(g, data, glyphs, width, height, payload);

        // Per blur: where its image is, for the shading.
        let backdrops: Vec<[u32; 4]> = plan
            .jobs
            .iter()
            .map(|j| {
                [
                    j.origin[0] | j.origin[1] << 16,
                    j.size[0] | j.size[1] << 16,
                    j.scale,
                    0,
                ]
            })
            .chain(core::iter::once([0; 4]))
            .collect();
        // Every layer but the last into a frame texture, the last into the target; each
        // layer's blurs from the one below.
        let externals: Vec<(ExternalImageId, TextureHandle<'g>)> = externals.to_vec();
        let mut below: Option<TextureHandle<'g>> = None;
        for l in 0..layers {
            let (first, end) = plan.range(l, commands);
            let images: Vec<(u32, TextureHandle<'g>)> = match below {
                Some(src) => plan
                    .jobs
                    .iter()
                    .enumerate()
                    .filter(|(_, j)| j.layer as usize == l && j.size[0] > 0)
                    .map(|(i, j)| (i as u32, self.add_blur(g, src, j, width, height)))
                    .collect(),
                None => Vec::new(),
            };
            let out = if l + 1 == layers {
                target
            } else {
                g.create_texture("UiLayer", TextureDesc::new_2d(width, height, UI_FORMAT))
            };
            let backdrops = backdrops.clone();
            let externals = externals.clone();
            let pipeline = &self.fine;
            let out = g.add_compute("UiFine", |b| {
                let h = b.read_buffer(heads, Access::ComputeRead);
                let p = b.read_buffer(pool, Access::ComputeRead);
                let gp = b.read_texture(pages.pages, Access::ComputeRead);
                let externals: Vec<(ExternalImageId, TextureHandle<'g>)> = externals
                    .iter()
                    .map(|&(id, t)| (id, b.read_texture(t, Access::ComputeRead)))
                    .collect();
                let images: Vec<(u32, TextureHandle<'g>)> = images
                    .iter()
                    .map(|&(i, t)| (i, b.read_texture(t, Access::ComputeRead)))
                    .collect();
                let previous = below.map(|t| b.read_texture(t, Access::ComputeRead));
                let path_buffers = path_tiles.buffers.map(|(t, e)| {
                    (
                        b.read_buffer(t, Access::ComputeRead),
                        b.read_buffer(e, Access::ComputeRead),
                    )
                });
                let path_records = path_tiles.records.clone();
                let out = b.write_texture(out.mip(0), Access::ComputeWrite);
                (out, move |ctx: &mut PassContext<'_>| {
                    // External images and blurred backdrops get their views now; one without a
                    // texture draws nothing.
                    let paints: Vec<[u32; 4]> = paints
                        .iter()
                        .map(|&p| match p[0] {
                            PAINT_EXTERNAL => match externals.iter().find(|(id, _)| id.0 == p[2]) {
                                Some(&(_, t)) => [p[0], p[1], ctx.srv(t), p[3]],
                                None => [p[0], 0, 0, p[3]],
                            },
                            PAINT_BACKDROP => match images.iter().find(|(i, _)| *i == p[2]) {
                                Some(&(_, t)) => [p[0], p[1], ctx.srv(t), p[3]],
                                None => [p[0], p[1], 0, p[3]],
                            },
                            _ => p,
                        })
                        .collect();
                    let args = FineArgs {
                        frame: ctx.upload(&frame),
                        commands: ctx.upload_slice(cmds),
                        payload: ctx.upload_slice(payload),
                        paints: ctx.upload_slice(&paints),
                        gradients: ctx.upload_slice(gradients),
                        glyphs: ctx.upload_slice(glyphs),
                        groups: ctx.upload_slice(groups),
                        clips: ctx.upload_slice(clips),
                        heads: ctx.srv(h),
                        pool: ctx.srv(p),
                        glyph_pages: ctx.srv(gp),
                        output: ctx.uav(out.mip(0)),
                        first,
                        end,
                        previous: previous.map_or(0, |t| ctx.srv(t)),
                        backdrops: ctx.upload_slice(&backdrops),
                        paths: ctx.upload_slice(&path_records),
                        path_tiles: 0,
                        path_entries: 0,
                    };
                    let args = match path_buffers {
                        Some((t, e)) => FineArgs {
                            path_tiles: ctx.srv(t),
                            path_entries: ctx.srv(e),
                            ..args
                        },
                        None => FineArgs {
                            path_tiles: ctx.upload_slice(&[0u32; 4]),
                            path_entries: ctx.upload_slice(&[[0u32; 4]]),
                            ..args
                        },
                    };
                    ctx.set_pipeline(pipeline);
                    ctx.push(&args);
                    ctx.dispatch(tiles_x, tiles_y, 1);
                })
            });
            if l + 1 == layers {
                target = out;
            } else {
                below = Some(out);
            }
        }
        // A texture surface's mips, each from the one above (§10).
        if let Some(t) = &surface.texture {
            for mip in 1..t.mips {
                let size = |m: u32| (t.width >> m).max(1) | (t.height >> m).max(1) << 16;
                let (w, h) = ((t.width >> mip).max(1), (t.height >> mip).max(1));
                let args = BlurArgs {
                    source: 0,
                    output: 0,
                    mode: 0,
                    scale: 2,
                    origin: 0,
                    size: size(mip),
                    source_size: size(mip - 1),
                    sigma: 0.0,
                };
                let pipeline = &self.blur;
                target = g.add_compute("UiMip", |b| {
                    let s = b.read_texture(target.mip(mip - 1), Access::ComputeRead);
                    let o = b.write_texture(target.mip(mip), Access::ComputeWrite);
                    (o, move |ctx: &mut PassContext<'_>| {
                        ctx.set_pipeline(pipeline);
                        ctx.push(&BlurArgs {
                            source: ctx.srv(s),
                            output: ctx.uav(o.mip(mip)),
                            ..args
                        });
                        ctx.dispatch(w.div_ceil(8), h.div_ceil(8), 1);
                    })
                });
            }
        }
        UiOutput {
            target,
            heads,
            pool,
            paths: path_tiles.buffers,
        }
    }

    /// The passes that give every path its tiles (§4.2): each segment counted in the tiles it
    /// crosses, the backdrops summed along the rows and the entries allocated, each segment
    /// listed. Beside the binning: they only read the payload.
    fn add_path_tiles<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        data: &UiFrameData,
        glyphs: &[[u32; 4]],
        width: u32,
        height: u32,
        payload: &'g [u32],
    ) -> PathTiles<'g> {
        let layout = crate::path::layout(&data.chunk, glyphs, width, height);
        let records: Vec<u32> = if layout.records.is_empty() {
            alloc::vec![0; crate::path::PATH_RECORD_WORDS]
        } else {
            layout.records.as_flattened().to_vec()
        };
        if layout.segment_paths.is_empty() {
            return PathTiles {
                records,
                buffers: None,
            };
        }
        let tiles = g.create_buffer(
            "UiPathTiles",
            BufferDesc::structured::<u32>(u64::from(layout.tiles) * PATH_TILE_WORDS as u64),
        );
        let entries = g.create_buffer(
            "UiPathEntries",
            BufferDesc::structured::<[u32; 4]>(u64::from(layout.entries.max(1))),
        );
        let counter = g.create_buffer("UiPathCounter", BufferDesc::structured::<u32>(1));
        let (mut tiles, counter) = g.add_compute("UiPathClear", |b| {
            let t = b.write_buffer(tiles, Access::Clear);
            let k = b.write_buffer(counter, Access::Clear);
            ((t, k), move |ctx: &mut PassContext<'_>| {
                ctx.fill_buffer(t, 0);
                ctx.fill_buffer(k, 0);
            })
        });
        let mut entries = entries;
        for (mode, name) in [(0, "UiPathCount"), (1, "UiPathAlloc"), (2, "UiPathFill")] {
            let items = if mode == 1 {
                layout.row_paths.clone()
            } else {
                layout.segment_paths.clone()
            };
            let count = items.len() as u32;
            let records = records.clone();
            let pipeline = &self.path_tiles;
            let capacity = layout.entries;
            (tiles, entries) = g.add_compute(name, |b| {
                let t = b.write_buffer(tiles, Access::ComputeWrite);
                let k = (mode == 1).then(|| b.write_buffer(counter, Access::ComputeWrite));
                let e = (mode == 2).then(|| b.write_buffer(entries, Access::ComputeWrite));
                (
                    (t, e.unwrap_or(entries)),
                    move |ctx: &mut PassContext<'_>| {
                        let args = PathTilesArgs {
                            payload: ctx.upload_slice(payload),
                            paths: ctx.upload_slice(&records),
                            items: ctx.upload_slice(&items),
                            tiles: ctx.uav(t),
                            counter: k.map_or(0, |k| ctx.uav(k)),
                            entries: e.map_or(0, |e| ctx.uav(e)),
                            entry_capacity: capacity,
                            mode,
                            count,
                        };
                        ctx.set_pipeline(pipeline);
                        ctx.push(&args);
                        let groups = count.div_ceil(256);
                        ctx.dispatch(groups.min(MAX_GROUPS), groups.div_ceil(MAX_GROUPS), 1);
                    },
                )
            });
        }
        PathTiles {
            records,
            buffers: Some((tiles, entries)),
        }
    }

    /// `job`'s blurred image of `src` (§5.6): scaled down, then blurred along x and along y.
    fn add_blur<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        src: TextureHandle<'g>,
        job: &BlurJob,
        width: u32,
        height: u32,
    ) -> TextureHandle<'g> {
        let [w, h] = job.size;
        let mut base = BlurArgs {
            source: 0,
            output: 0,
            mode: 0,
            scale: job.scale,
            origin: job.origin[0] | job.origin[1] << 16,
            size: w | h << 16,
            source_size: width | height << 16,
            sigma: job.sigma,
        };
        let mut texture = src;
        for (mode, name) in [(0, "UiBlurDown"), (1, "UiBlurX"), (2, "UiBlurY")] {
            base.mode = mode;
            if mode == 1 {
                base.source_size = base.size;
            }
            let args = base;
            let out = g.create_texture(name, TextureDesc::new_2d(w, h, UI_FORMAT));
            let pipeline = &self.blur;
            texture = g.add_compute(name, |b| {
                let s = b.read_texture(texture, Access::ComputeRead);
                let o = b.write_texture(out, Access::ComputeWrite);
                (o, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(pipeline);
                    ctx.push(&BlurArgs {
                        source: ctx.srv(s),
                        output: ctx.uav(o),
                        ..args
                    });
                    ctx.dispatch(w.div_ceil(8), h.div_ceil(8), 1);
                })
            });
        }
        texture
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
    }
}
