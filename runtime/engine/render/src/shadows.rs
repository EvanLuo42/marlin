//! Virtual shadow maps for the sun (0010 §5.3): a clipmap of levels around the camera, each
//! [`VsmSettings::virtual_resolution`]² texels in pages of [`VsmSettings::page_size`]², level k
//! twice the extent of level k − 1. Only the pages the frame's surfaces sample get a physical
//! page of the pool; each level's pages are drawn by the visibility passes in orthographic mode
//! ([`Visibility::add_shadow_view`]); the result is a screen-space sun shadow mask the lighting
//! multiplies the sun by. The passes are `Marlin.Engine.Shadows`'s; its file comment describes
//! them.
//!
//! Every level's center is the camera's position across the light, snapped to the level's
//! pages, so a texel stays on the same world spot while the camera moves. Light depth is about
//! 0.5 at the camera (the depth center snaps in eighths of [`VsmSettings::depth_range`]),
//! larger toward the light.
//!
//! # Caching (R2g)
//!
//! Pages persist across frames, per view: the physical pool, a cache table (each virtual page's
//! last physical page) and each physical page's tag (its level, its absolute page across the
//! light, the generation it was drawn in). A level's pages are world-anchored and wrap around
//! the table, so a page keeps its slot as the level follows the camera. A frame draws only the
//! pages it marks that are not cached: new ones, and those invalidated by
//!
//! - a world sphere whose shadow changed: an instance moved, appeared, vanished, was hidden or
//!   shown, changed mesh ([`RenderScene::shadow_changes`]), or its mesh streamed pages in or
//!   out ([`Meshes::changed_meshes`]: its LOD cut may differ);
//! - a new generation: the sun turned, the depth center moved a step, or the settings changed.
//!
//! Free pages for new ones are those holding nothing valid first, then cached pages nothing
//! marked this frame.
//!
//! Not yet (a later R2 step): SMRT soft shadows (a 3×3 PCF filters every level).

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use marlin_jobs::sync::Mutex;

use marlin_math::{Mat4, Transform, Vec3, Vec4};
use marlin_render_graph::{
    Access, BufferDesc, BufferHandle, PassContext, RenderGraph, TextureDesc, TextureHandle,
};
use marlin_rhi::{ComputePipeline, ComputePipelineDesc, Device, Format, Result, ShaderModule};
use marlin_serial::cooked::Pod;
use marlin_shader::asset::ShaderLibrary;
use marlin_shader::bind::{BindError, EntryDesc, bind};

use crate::Meshes;
use crate::meshes::{Camera, Environment};
use crate::scene::{GpuScene, RenderScene};
use crate::visibility::{ShadowView, Visibility};

/// The shadow passes' shaders (`Marlin.Engine.Shadows`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VsmShaders<'a> {
    pub mark: ShaderModule<'a>,
    pub invalidate: ShaderModule<'a>,
    pub keep: ShaderModule<'a>,
    pub free_list: ShaderModule<'a>,
    pub allocate: ShaderModule<'a>,
    pub clear_args: ShaderModule<'a>,
    pub clear: ShaderModule<'a>,
    pub mask: ShaderModule<'a>,
}

impl<'a> VsmShaders<'a> {
    /// The entries, in field order: every shadow pass sets `VsmArgs`.
    pub const ENTRIES: [EntryDesc; 8] = [
        EntryDesc::compute("Marlin.Engine.Shadows.VsmMark").push::<VsmArgs>(),
        EntryDesc::compute("Marlin.Engine.Shadows.VsmInvalidate").push::<VsmArgs>(),
        EntryDesc::compute("Marlin.Engine.Shadows.VsmKeep").push::<VsmArgs>(),
        EntryDesc::compute("Marlin.Engine.Shadows.VsmFreeList").push::<VsmArgs>(),
        EntryDesc::compute("Marlin.Engine.Shadows.VsmAllocate").push::<VsmArgs>(),
        EntryDesc::compute("Marlin.Engine.Shadows.VsmClearArgs").push::<VsmArgs>(),
        EntryDesc::compute("Marlin.Engine.Shadows.VsmClear").push::<VsmArgs>(),
        EntryDesc::compute("Marlin.Engine.Shadows.VsmMask").push::<VsmArgs>(),
    ];

    /// The shaders from `modules`, in [`ENTRIES`](Self::ENTRIES) order.
    pub fn from_modules(modules: [ShaderModule<'a>; 8]) -> Self {
        let [
            mark,
            invalidate,
            keep,
            free_list,
            allocate,
            clear_args,
            clear,
            mask,
        ] = modules;
        Self {
            mark,
            invalidate,
            keep,
            free_list,
            allocate,
            clear_args,
            clear,
            mask,
        }
    }

    /// The shadow passes' shaders from `library`, checked against what they set.
    pub fn bind(library: &'a ShaderLibrary) -> core::result::Result<Self, BindError> {
        bind(library, &Self::ENTRIES).map(Self::from_modules)
    }
}

/// The format of the sun shadow mask: 1 lit, 0 in shadow.
pub const SHADOW_MASK_FORMAT: Format = Format::R16Float;

/// How big the clipmap and the pool are.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VsmSettings {
    pub levels: u32,
    /// Texels per side of a level.
    pub virtual_resolution: u32,
    /// Texels per side of a page (a multiple of 8).
    pub page_size: u32,
    /// Level 0's texel, in world units.
    pub first_texel: f32,
    /// The light depth the levels hold, centered near the camera, in world units.
    pub depth_range: f32,
    /// Physical pages per row and rows of the pool.
    pub pool_pages: [u32; 2],
    /// How far a surface is pushed along its normal before it is tested, and how much nearer
    /// the light its depth is taken: in texels of its level.
    pub normal_offset: f32,
    pub depth_bias: f32,
}

impl VsmSettings {
    /// The desktop default (0010 §5.3): 16K² levels of 128² pages, an 8K² pool; level 0's
    /// half-millimeter texels for close-ups (8 m across), 14 levels to 65 km.
    pub const DEFAULT: Self = Self {
        levels: 14,
        virtual_resolution: 16384,
        page_size: 128,
        first_texel: 8.0 / 16384.0,
        depth_range: 2000.0,
        pool_pages: [64, 64],
        normal_offset: 2.0,
        depth_bias: 1.5,
    };

    /// Pages per side of a level.
    pub const fn pages_per_side(&self) -> u32 {
        self.virtual_resolution / self.page_size
    }

    /// Virtual pages of every level: the page table's entries.
    pub const fn virtual_pages(&self) -> u32 {
        self.levels * self.pages_per_side() * self.pages_per_side()
    }

    /// Pages of the physical pool.
    pub const fn physical_pages(&self) -> u32 {
        self.pool_pages[0] * self.pool_pages[1]
    }

    /// The step the light depth's center snaps in: an eighth of the range.
    pub fn depth_step(&self) -> f32 {
        self.depth_range / 8.0
    }
}

impl Default for VsmSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// `Marlin.Engine.Shadows.VsmLevel`.
#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct Level {
    view_projection: [f32; 16],
    texel: f32,
    spare: u32,
    origin: [i32; 2],
    offset: [u32; 2],
}

/// `Marlin.Engine.Shadows.VsmConstants`.
#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct Constants {
    inverse_view_projection: [f32; 16],
    eye: [f32; 4],
    width: u32,
    height: u32,
    levels: u32,
    virtual_resolution: u32,
    page_size: u32,
    pages_per_side: u32,
    pool_pages_per_row: u32,
    pool_pages: u32,
    first_texel: f32,
    depth_per_unit: f32,
    normal_offset: f32,
    depth_bias: f32,
    normals: u32,
    generation: u32,
    invalidation_count: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy, Default)]
struct VsmArgs {
    constants: u32,
    levels: u32,
    depth: u32,
    material: u32,
    requests: u32,
    page_table: u32,
    counters: u32,
    args: u32,
    pool: u32,
    mask: u32,
    stored: u32,
    cache: u32,
    tags: u32,
    keep: u32,
    free: u32,
    invalidations: u32,
}

/// The words of [`VsmOutput::counters`].
pub mod counters {
    /// Marked pages that were not cached: drawn this frame, as far as the pool had room.
    pub const DRAWN: usize = 0;
    /// Free physical pages: holding nothing valid, and cached ones nothing marked.
    pub const FREE_EMPTY: usize = 1;
    pub const FREE_CACHED: usize = 2;
    /// Marked pages found cached.
    pub const CACHED: usize = 3;
}

/// A page table entry's bit: the page is drawn this frame.
pub const RENDER_BIT: u32 = 1 << 31;

/// What [`Vsm::add_passes`] makes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VsmOutput<'g> {
    /// [`SHADOW_MASK_FORMAT`], the screen's size: the sun's visibility.
    pub mask: TextureHandle<'g>,
    /// The used-slot marks after the shadow views read their pages too.
    pub used: BufferHandle<'g>,
    /// The page table (a u32 per table slot of every level: its physical page, with
    /// [`RENDER_BIT`] if drawn this frame, or `u32::MAX`), the physical pool (`R32Uint` depth
    /// bits) and the [`counters`], for tools and tests.
    pub page_table: BufferHandle<'g>,
    pub pool: TextureHandle<'g>,
    pub counters: BufferHandle<'g>,
}

/// A level of the clipmap for a frame.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ClipLevel {
    /// World to the level's clip space.
    pub view_projection: Mat4,
    /// Its texel, in world units.
    pub texel: f32,
    /// The absolute page (across the light: x right, y down) of its first page.
    pub origin: [i32; 2],
}

/// The sun's levels for a frame: the camera at `eye`, the light depth centered at
/// `depth_center` (world units along the light).
pub fn levels(
    settings: &VsmSettings,
    eye: Vec3,
    light_direction: Vec3,
    depth_center: f32,
) -> Vec<ClipLevel> {
    let f = light_direction.normalize_or_zero();
    let f = if f == Vec3::ZERO { -Vec3::Y } else { f };
    let reference = if f.y.abs() < 0.99 { Vec3::Y } else { Vec3::X };
    let right = f.cross(reference).normalize();
    let up = right.cross(f);
    let depth_per_unit = 1.0 / settings.depth_range;
    let n = settings.pages_per_side() as i32;
    (0..settings.levels)
        .map(|k| {
            let texel = settings.first_texel * (1u64 << k) as f32;
            let extent = texel * settings.virtual_resolution as f32;
            // Snapped to the level's pages across the light.
            let page = extent / settings.pages_per_side() as f32;
            let ix = marlin_math::scalar::round(eye.dot(right) / page) as i32;
            let iy = marlin_math::scalar::round(eye.dot(up) / page) as i32;
            let center = right * (ix as f32 * page) + up * (iy as f32 * page) + f * depth_center;
            let s = 2.0 / extent;
            let vp = Mat4::from_cols(
                Vec4::new(right.x * s, up.x * s, -f.x * depth_per_unit, 0.0),
                Vec4::new(right.y * s, up.y * s, -f.y * depth_per_unit, 0.0),
                Vec4::new(right.z * s, up.z * s, -f.z * depth_per_unit, 0.0),
                Vec4::new(
                    -center.dot(right) * s,
                    -center.dot(up) * s,
                    0.5 + center.dot(f) * depth_per_unit,
                    1.0,
                ),
            );
            ClipLevel {
                view_projection: vp,
                texel,
                origin: [ix - n / 2, -iy - n / 2],
            }
        })
        .collect()
}

/// What a view's cached pages were drawn with.
#[derive(Clone, Copy, PartialEq)]
struct ViewCache {
    light: Vec3,
    depth_step: i64,
    settings: VsmSettings,
    generation: u32,
}

/// The shadow passes' pipelines, the clipmap's settings and each view's cache state.
pub struct Vsm {
    mark: ComputePipeline,
    invalidate: ComputePipeline,
    keep: ComputePipeline,
    free_list: ComputePipeline,
    allocate: ComputePipeline,
    clear_args: ComputePipeline,
    clear: ComputePipeline,
    mask: ComputePipeline,
    pub settings: VsmSettings,
    caches: Mutex<BTreeMap<u64, ViewCache>>,
}

impl Vsm {
    pub fn new(device: &Device, shaders: &VsmShaders<'_>, settings: VsmSettings) -> Result<Self> {
        let compute = |shader| {
            device.create_compute_pipeline(&ComputePipelineDesc {
                shader,
                cache: None,
            })
        };
        Ok(Self {
            mark: compute(shaders.mark)?,
            invalidate: compute(shaders.invalidate)?,
            keep: compute(shaders.keep)?,
            free_list: compute(shaders.free_list)?,
            allocate: compute(shaders.allocate)?,
            clear_args: compute(shaders.clear_args)?,
            clear: compute(shaders.clear)?,
            mask: compute(shaders.mask)?,
            settings,
            caches: Mutex::new(BTreeMap::new()),
        })
    }

    /// The generation of `view`'s pages this frame, and the light depth's center: a new
    /// generation when the sun, the depth center's step or the settings changed.
    fn generation(&self, view: u64, eye: Vec3, light: Vec3) -> (u32, f32) {
        let s = self.settings;
        let f = light.normalize_or_zero();
        let f = if f == Vec3::ZERO { -Vec3::Y } else { f };
        let step = marlin_math::scalar::round(eye.dot(f) / s.depth_step()) as i64;
        let mut caches = self.caches.lock();
        let generation = match caches.get(&view) {
            Some(c) if c.light == light && c.depth_step == step && c.settings == s => c.generation,
            Some(c) => c.generation.wrapping_add(1),
            None => 0,
        };
        caches.insert(
            view,
            ViewCache {
                light,
                depth_step: step,
                settings: s,
                generation,
            },
        );
        (generation, step as f32 * s.depth_step())
    }

    /// Declares the sun's shadow of the view `view`, the camera `camera` (`width` × `height`):
    /// pages marked from its `depth` (`R32Float`, the visibility passes' split depth), those
    /// not cached allocated and drawn by `vis` (reading the GPU Scene `gpu`, the page pool
    /// `pages`, marking `used`), and the shadow mask filtered with the surfaces' normals from
    /// `material` (the material buffer) if there is one. The cache is `view`'s; `scene` and
    /// `meshes` are as last applied, their changes invalidating cached pages.
    #[allow(clippy::too_many_arguments, reason = "one view's inputs")]
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        view: u64,
        vis: &'g Visibility,
        meshes: &Meshes,
        scene: &RenderScene,
        gpu: GpuScene<'g>,
        pages: BufferHandle<'g>,
        used: BufferHandle<'g>,
        depth: TextureHandle<'g>,
        material: Option<TextureHandle<'g>>,
        env: &Environment,
        (eye, camera): (Transform, Camera),
        width: u32,
        height: u32,
    ) -> VsmOutput<'g> {
        let s = self.settings;
        let n = s.pages_per_side();
        let virtual_pages = s.virtual_pages();
        let pool_pages = s.physical_pages();
        let aspect = width as f32 / height.max(1) as f32;
        let view_projection = Mat4::perspective_infinite_rh(camera.fov_y, aspect, camera.near)
            * eye.to_matrix().inverse_affine();
        let (generation, depth_center) =
            self.generation(view, eye.translation, env.light_direction);
        let levels = levels(&s, eye.translation, env.light_direction, depth_center);
        let wrap = |o: i32| o.rem_euclid(n as i32) as u32;
        let level_records: Vec<Level> = levels
            .iter()
            .map(|l| Level {
                view_projection: l.view_projection.to_cols_array(),
                texel: l.texel,
                spare: 0,
                origin: l.origin,
                offset: l.origin.map(wrap),
            })
            .collect();
        // The spheres whose shadows changed since the last frame.
        let mut invalidations: Vec<[f32; 4]> = scene.shadow_changes().to_vec();
        invalidations.extend(scene.spheres_of(meshes.changed_meshes()));
        let p = eye.translation;
        let constants = Constants {
            inverse_view_projection: view_projection.inverse().to_cols_array(),
            eye: [
                p.x,
                p.y,
                p.z,
                2.0 * marlin_math::scalar::tan(camera.fov_y * 0.5) / height.max(1) as f32,
            ],
            width,
            height,
            levels: s.levels,
            virtual_resolution: s.virtual_resolution,
            page_size: s.page_size,
            pages_per_side: n,
            pool_pages_per_row: s.pool_pages[0],
            pool_pages,
            first_texel: s.first_texel,
            depth_per_unit: 1.0 / s.depth_range,
            normal_offset: s.normal_offset,
            depth_bias: s.depth_bias,
            normals: u32::from(material.is_some()),
            generation,
            invalidation_count: invalidations.len() as u32,
        };

        // Persistent: the pool, the cache table and the tags.
        let pool = g.persistent(
            "VsmPool",
            view,
            TextureDesc::new_2d(
                s.pool_pages[0] * s.page_size,
                s.pool_pages[1] * s.page_size,
                Format::R32Uint,
            ),
        );
        let cache = g.persistent(
            "VsmCache",
            view,
            BufferDesc::structured::<u32>(u64::from(virtual_pages)),
        );
        let tags = g.persistent(
            "VsmTags",
            view,
            BufferDesc::structured::<[u32; 4]>(u64::from(pool_pages)),
        );
        let fresh = pool.fresh || cache.fresh || tags.fresh;
        let (mut pool, mut cache, mut tags) = (pool.handle, cache.handle, tags.handle);
        if fresh {
            (cache, tags) = g.add_compute("VsmCacheReset", |b| {
                let cache = b.write_buffer(cache, Access::Clear);
                let tags = b.write_buffer(tags, Access::Clear);
                ((cache, tags), move |ctx: &mut PassContext<'_>| {
                    ctx.fill_buffer(cache, u32::MAX);
                    ctx.fill_buffer(tags, u32::MAX);
                })
            });
        }

        let requests = g.create_buffer(
            "VsmRequests",
            BufferDesc::structured::<u32>(u64::from(virtual_pages)),
        );
        let page_table = g.create_buffer(
            "VsmPageTable",
            BufferDesc::structured::<u32>(u64::from(virtual_pages)),
        );
        let keep = g.create_buffer(
            "VsmKeep",
            BufferDesc::structured::<u32>(u64::from(pool_pages)),
        );
        let free = g.create_buffer(
            "VsmFree",
            BufferDesc::structured::<u32>(2 * u64::from(pool_pages)),
        );
        let counters = g.create_buffer("VsmCounters", BufferDesc::structured::<u32>(4));
        let args = g.create_buffer("VsmArgs", BufferDesc::structured::<u32>(4));
        let mask = g.create_texture(
            "SunShadowMask",
            TextureDesc::new_2d(width, height, SHADOW_MASK_FORMAT),
        );

        let (requests, keep, counters) = g.add_compute("VsmReset", |b| {
            let requests = b.write_buffer(requests, Access::Clear);
            let keep = b.write_buffer(keep, Access::Clear);
            let counters = b.write_buffer(counters, Access::Clear);
            (
                (requests, keep, counters),
                move |ctx: &mut PassContext<'_>| {
                    ctx.fill_buffer(requests, 0);
                    ctx.fill_buffer(keep, 0);
                    ctx.fill_buffer(counters, 0);
                },
            )
        });
        let screen = (width.div_ceil(8), height.div_ceil(8));
        let requests = g.add_compute("VsmMark", |b| {
            let depth = b.read_texture(depth, Access::ComputeRead);
            let requests = b.write_buffer(requests, Access::ComputeWrite);
            let records = level_records.clone();
            (requests, move |ctx: &mut PassContext<'_>| {
                let args = VsmArgs {
                    constants: ctx.upload(&constants),
                    levels: ctx.upload_slice(&records),
                    depth: ctx.srv(depth),
                    requests: ctx.uav(requests),
                    ..VsmArgs::default()
                };
                ctx.set_pipeline(&self.mark);
                ctx.push(&args);
                ctx.dispatch(screen.0, screen.1, 1);
            })
        });
        if !invalidations.is_empty() {
            tags = g.add_compute("VsmInvalidate", |b| {
                let tags = b.write_buffer(tags, Access::ComputeWrite);
                let records = level_records.clone();
                (tags, move |ctx: &mut PassContext<'_>| {
                    let args = VsmArgs {
                        constants: ctx.upload(&constants),
                        levels: ctx.upload_slice(&records),
                        tags: ctx.uav(tags),
                        invalidations: ctx.upload_slice(&invalidations),
                        ..VsmArgs::default()
                    };
                    ctx.set_pipeline(&self.invalidate);
                    ctx.push(&args);
                    ctx.dispatch(pool_pages.div_ceil(64), 1, 1);
                })
            });
        }
        let keep = g.add_compute("VsmKeep", |b| {
            let requests = b.read_buffer(requests, Access::ComputeRead);
            let cache = b.read_buffer(cache, Access::ComputeRead);
            let tags = b.read_buffer(tags, Access::ComputeRead);
            let keep = b.write_buffer(keep, Access::ComputeWrite);
            let records = level_records.clone();
            (keep, move |ctx: &mut PassContext<'_>| {
                let args = VsmArgs {
                    constants: ctx.upload(&constants),
                    levels: ctx.upload_slice(&records),
                    requests: ctx.srv(requests),
                    cache: ctx.srv(cache),
                    tags: ctx.srv(tags),
                    keep: ctx.uav(keep),
                    ..VsmArgs::default()
                };
                ctx.set_pipeline(&self.keep);
                ctx.push(&args);
                ctx.dispatch(virtual_pages.div_ceil(64), 1, 1);
            })
        });
        let (free, counters) = g.add_compute("VsmFreeList", |b| {
            let keep = b.read_buffer(keep, Access::ComputeRead);
            let tags = b.read_buffer(tags, Access::ComputeRead);
            let free = b.write_buffer(free, Access::ComputeWrite);
            let counters = b.write_buffer(counters, Access::ComputeWrite);
            ((free, counters), move |ctx: &mut PassContext<'_>| {
                let args = VsmArgs {
                    constants: ctx.upload(&constants),
                    keep: ctx.srv(keep),
                    tags: ctx.srv(tags),
                    free: ctx.uav(free),
                    counters: ctx.uav(counters),
                    ..VsmArgs::default()
                };
                ctx.set_pipeline(&self.free_list);
                ctx.push(&args);
                ctx.dispatch(pool_pages.div_ceil(64), 1, 1);
            })
        });
        let (page_table, cache, tags, counters) = g.add_compute("VsmAllocate", |b| {
            let requests = b.read_buffer(requests, Access::ComputeRead);
            let keep = b.read_buffer(keep, Access::ComputeRead);
            let free = b.read_buffer(free, Access::ComputeRead);
            let page_table = b.write_buffer(page_table, Access::ComputeWrite);
            let cache = b.write_buffer(cache, Access::ComputeWrite);
            let tags = b.write_buffer(tags, Access::ComputeWrite);
            let counters = b.write_buffer(counters, Access::ComputeWrite);
            let records = level_records.clone();
            (
                (page_table, cache, tags, counters),
                move |ctx: &mut PassContext<'_>| {
                    let args = VsmArgs {
                        constants: ctx.upload(&constants),
                        levels: ctx.upload_slice(&records),
                        requests: ctx.srv(requests),
                        keep: ctx.srv(keep),
                        free: ctx.srv(free),
                        page_table: ctx.uav(page_table),
                        cache: ctx.uav(cache),
                        tags: ctx.uav(tags),
                        counters: ctx.uav(counters),
                        ..VsmArgs::default()
                    };
                    ctx.set_pipeline(&self.allocate);
                    ctx.push(&args);
                    ctx.dispatch(virtual_pages.div_ceil(64), 1, 1);
                },
            )
        });
        // The cache and the tags are written for the next frame.
        let _ = (cache, tags);
        let args = g.add_compute("VsmClearArgs", |b| {
            let counters = b.read_buffer(counters, Access::ComputeRead);
            let args = b.write_buffer(args, Access::ComputeWrite);
            (args, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.clear_args);
                let constants = ctx.upload(&constants);
                ctx.push(&VsmArgs {
                    constants,
                    counters: ctx.srv(counters),
                    args: ctx.uav(args),
                    ..VsmArgs::default()
                });
                ctx.dispatch(1, 1, 1);
            })
        });
        pool = g.add_compute("VsmClear", |b| {
            let dispatch = b.read_buffer(args, Access::Indirect);
            let counters = b.read_buffer(counters, Access::ComputeRead);
            let free = b.read_buffer(free, Access::ComputeRead);
            let pool = b.write_texture(pool, Access::ComputeWrite);
            (pool, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.clear);
                let constants = ctx.upload(&constants);
                ctx.push(&VsmArgs {
                    constants,
                    counters: ctx.srv(counters),
                    free: ctx.srv(free),
                    pool: ctx.uav(pool),
                    ..VsmArgs::default()
                });
                ctx.dispatch_indirect(dispatch, 1, None);
            })
        });

        // Each level's pages drawn this frame.
        let mut used = used;
        for (k, (level, record)) in levels.iter().zip(&level_records).enumerate() {
            let view = ShadowView {
                view_projection: level.view_projection,
                texels_per_unit: 1.0 / level.texel,
                resolution: s.virtual_resolution,
                page_size: s.page_size,
                pages_per_side: n,
                page_base: k as u32 * n * n,
                pool_pages_per_row: s.pool_pages[0],
                page_offset: record.offset,
            };
            (used, pool) =
                vis.add_shadow_view(g, meshes, scene, gpu, pages, used, page_table, pool, &view);
        }

        let mask = g.add_compute("VsmMask", |b| {
            let depth = b.read_texture(depth, Access::ComputeRead);
            let material = material.map(|m| b.read_texture(m, Access::ComputeRead));
            let page_table = b.read_buffer(page_table, Access::ComputeRead);
            let pool = b.read_texture(pool, Access::ComputeRead);
            let mask = b.write_texture(mask, Access::ComputeWrite);
            let records = level_records;
            (mask, move |ctx: &mut PassContext<'_>| {
                let args = VsmArgs {
                    constants: ctx.upload(&constants),
                    levels: ctx.upload_slice(&records),
                    depth: ctx.srv(depth),
                    material: material.map_or(0, |m| ctx.srv(m)),
                    page_table: ctx.srv(page_table),
                    stored: ctx.srv(pool),
                    mask: ctx.uav(mask),
                    ..VsmArgs::default()
                };
                ctx.set_pipeline(&self.mask);
                ctx.push(&args);
                ctx.dispatch(screen.0, screen.1, 1);
            })
        });
        VsmOutput {
            mask,
            used,
            page_table,
            pool,
            counters,
        }
    }
}
