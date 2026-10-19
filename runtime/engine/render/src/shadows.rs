//! Virtual shadow maps for the sun (0010 §5.3, R2f): a clipmap of levels around the camera,
//! each [`VsmSettings::virtual_resolution`]² texels in pages of [`VsmSettings::page_size`]²,
//! level k twice the extent of level k − 1. Only the pages the frame's surfaces sample get a
//! physical page of the pool; each level's mapped pages are drawn by the visibility passes in
//! orthographic mode ([`Visibility::add_shadow_view`]); the result is a screen-space sun
//! shadow mask the lighting multiplies the sun by. The passes are `Marlin.Engine.Shadows`'s;
//! its file comment describes them.
//!
//! Every level's center is the camera's position across the light, snapped to the level's
//! pages, so a texel stays on the same world spot while the camera moves. Light depth is 0.5 at
//! the camera, larger toward the light, over [`VsmSettings::depth_range`].
//!
//! Not yet (later R2 steps): keeping static pages across frames (every mapped page is drawn
//! each frame), and SMRT soft shadows (a 3×3 PCF filters every level).

use alloc::vec::Vec;

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
    pub allocate: ShaderModule<'a>,
    pub clear_args: ShaderModule<'a>,
    pub clear: ShaderModule<'a>,
    pub mask: ShaderModule<'a>,
}

impl<'a> VsmShaders<'a> {
    /// The entries, in field order: every shadow pass sets `VsmArgs`.
    pub const ENTRIES: [EntryDesc; 5] = [
        EntryDesc::compute("Marlin.Engine.Shadows.VsmMark").push::<VsmArgs>(),
        EntryDesc::compute("Marlin.Engine.Shadows.VsmAllocate").push::<VsmArgs>(),
        EntryDesc::compute("Marlin.Engine.Shadows.VsmClearArgs").push::<VsmArgs>(),
        EntryDesc::compute("Marlin.Engine.Shadows.VsmClear").push::<VsmArgs>(),
        EntryDesc::compute("Marlin.Engine.Shadows.VsmMask").push::<VsmArgs>(),
    ];

    /// The shaders from `modules`, in [`ENTRIES`](Self::ENTRIES) order.
    pub fn from_modules([mark, allocate, clear_args, clear, mask]: [ShaderModule<'a>; 5]) -> Self {
        Self {
            mark,
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
    /// The light depth the levels hold, centered on the camera, in world units.
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
    spare: [u32; 3],
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
}

/// What [`Vsm::add_passes`] makes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VsmOutput<'g> {
    /// [`SHADOW_MASK_FORMAT`], the screen's size: the sun's visibility.
    pub mask: TextureHandle<'g>,
    /// The used-slot marks after the shadow views read their pages too.
    pub used: BufferHandle<'g>,
    /// The page table (a u32 per virtual page of every level: its physical page or
    /// `u32::MAX`), the physical pool (`R32Uint` depth bits) and the counters (word 0: the
    /// pages marked), for tools and tests.
    pub page_table: BufferHandle<'g>,
    pub pool: TextureHandle<'g>,
    pub counters: BufferHandle<'g>,
}

/// The sun's levels for a frame: each level's view-projection and texel.
pub fn levels(settings: &VsmSettings, eye: Vec3, light_direction: Vec3) -> Vec<(Mat4, f32)> {
    let f = light_direction.normalize_or_zero();
    let f = if f == Vec3::ZERO { -Vec3::Y } else { f };
    let reference = if f.y.abs() < 0.99 { Vec3::Y } else { Vec3::X };
    let right = f.cross(reference).normalize();
    let up = right.cross(f);
    let depth_per_unit = 1.0 / settings.depth_range;
    (0..settings.levels)
        .map(|k| {
            let texel = settings.first_texel * (1u64 << k) as f32;
            let extent = texel * settings.virtual_resolution as f32;
            // Snapped to the level's pages across the light.
            let page = extent / settings.pages_per_side() as f32;
            let snap = |x: f32| marlin_math::scalar::round(x / page) * page;
            let center = right * snap(eye.dot(right)) + up * snap(eye.dot(up)) + f * eye.dot(f);
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
            (vp, texel)
        })
        .collect()
}

/// The shadow passes' pipelines and the clipmap's settings.
pub struct Vsm {
    mark: ComputePipeline,
    allocate: ComputePipeline,
    clear_args: ComputePipeline,
    clear: ComputePipeline,
    mask: ComputePipeline,
    pub settings: VsmSettings,
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
            allocate: compute(shaders.allocate)?,
            clear_args: compute(shaders.clear_args)?,
            clear: compute(shaders.clear)?,
            mask: compute(shaders.mask)?,
            settings,
        })
    }

    /// Declares the sun's shadow of the view `camera` (`width` × `height`): pages marked from
    /// its `depth` (`R32Float`, the visibility passes' split depth), allocated, drawn by `vis`
    /// (reading the GPU Scene `gpu`, the page pool `pages`, marking `used`), and the shadow mask
    /// filtered with the surfaces' normals from `material` (the material buffer) if there is one.
    #[allow(clippy::too_many_arguments, reason = "one view's inputs")]
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
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
        let pool_pages = s.pool_pages[0] * s.pool_pages[1];
        let aspect = width as f32 / height.max(1) as f32;
        let view_projection = Mat4::perspective_infinite_rh(camera.fov_y, aspect, camera.near)
            * eye.to_matrix().inverse_affine();
        let levels = levels(&s, eye.translation, env.light_direction);
        let level_records: Vec<Level> = levels
            .iter()
            .map(|(vp, texel)| Level {
                view_projection: vp.to_cols_array(),
                texel: *texel,
                spare: [0; 3],
            })
            .collect();
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
        };

        let requests = g.create_buffer(
            "VsmRequests",
            BufferDesc::structured::<u32>(u64::from(virtual_pages)),
        );
        let page_table = g.create_buffer(
            "VsmPageTable",
            BufferDesc::structured::<u32>(u64::from(virtual_pages)),
        );
        let counters = g.create_buffer("VsmCounters", BufferDesc::structured::<u32>(4));
        let args = g.create_buffer("VsmArgs", BufferDesc::structured::<u32>(4));
        let pool = g.create_texture(
            "VsmPool",
            TextureDesc::new_2d(
                s.pool_pages[0] * s.page_size,
                s.pool_pages[1] * s.page_size,
                Format::R32Uint,
            ),
        );
        let mask = g.create_texture(
            "SunShadowMask",
            TextureDesc::new_2d(width, height, SHADOW_MASK_FORMAT),
        );

        let (requests, counters) = g.add_compute("VsmReset", |b| {
            let requests = b.write_buffer(requests, Access::Clear);
            let counters = b.write_buffer(counters, Access::Clear);
            ((requests, counters), move |ctx: &mut PassContext<'_>| {
                ctx.fill_buffer(requests, 0);
                ctx.fill_buffer(counters, 0);
            })
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
        let (page_table, counters) = g.add_compute("VsmAllocate", |b| {
            let requests = b.read_buffer(requests, Access::ComputeRead);
            let page_table = b.write_buffer(page_table, Access::ComputeWrite);
            let counters = b.write_buffer(counters, Access::ComputeWrite);
            ((page_table, counters), move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.allocate);
                let constants = ctx.upload(&constants);
                ctx.push(&VsmArgs {
                    constants,
                    requests: ctx.srv(requests),
                    page_table: ctx.uav(page_table),
                    counters: ctx.uav(counters),
                    ..VsmArgs::default()
                });
                ctx.dispatch(virtual_pages.div_ceil(64), 1, 1);
            })
        });
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
        let mut pool = g.add_compute("VsmClear", |b| {
            let dispatch = b.read_buffer(args, Access::Indirect);
            let pool = b.write_texture(pool, Access::ComputeWrite);
            (pool, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.clear);
                let constants = ctx.upload(&constants);
                ctx.push(&VsmArgs {
                    constants,
                    pool: ctx.uav(pool),
                    ..VsmArgs::default()
                });
                ctx.dispatch_indirect(dispatch, 1, None);
            })
        });

        // Each level's mapped pages.
        let mut used = used;
        for (k, (vp, texel)) in levels.iter().enumerate() {
            let view = ShadowView {
                view_projection: *vp,
                texels_per_unit: 1.0 / texel,
                resolution: s.virtual_resolution,
                page_size: s.page_size,
                pages_per_side: n,
                page_base: k as u32 * n * n,
                pool_pages_per_row: s.pool_pages[0],
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
