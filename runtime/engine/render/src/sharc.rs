//! The world radiance cache (0010 §4.1, after NVIDIA's SHaRC; R4b): a hash grid of world-space
//! voxels holding the radiance their surfaces reflect, shared by every view. Each frame
//! [`Sharc::add_passes`] traces paths from a few screen pixels (`SharcUpdate`), adds each
//! vertex's reflected radiance to its voxel, then folds the frame's sums into the voxels'
//! averages and frees voxels nothing updated for a while (`SharcResolve`). `Lighting/Sharc.mshader`
//! has the passes and `SharcQuery`, what readers call.
//!
//! The sums are 64-bit fixed-point atomics ([`Features::atomic_int64`]): exact, and independent
//! of the exposure, which SHaRC's 32-bit sums are scaled by. Every platform with ray queries has
//! them.
//!
//! [`Features::atomic_int64`]: marlin_rhi::Features::atomic_int64

use marlin_math::Transform;
use marlin_render_graph::{
    Access, BufferDesc, BufferHandle, GLOBAL_SCOPE, PassBuilder, PassContext, RenderGraph,
    TextureDesc, TextureHandle,
};
use marlin_rhi::{ComputePipeline, ComputePipelineDesc, Device, Result, ShaderModule};
use marlin_serial::cooked::Pod;
use marlin_shader::asset::ShaderLibrary;
use marlin_shader::bind::{BindError, EntryDesc, bind};

use crate::lights::{LIGHT_TILE_SIZE, LIGHT_TILES, LightTileOutput};
use crate::raytracing::{HitSources, RtScene, SimpleOutput, SimpleSources};
use crate::{Camera, Environment};

/// The cache's compute shaders.
pub struct SharcShaders<'a> {
    pub update: ShaderModule<'a>,
    pub resolve: ShaderModule<'a>,
    pub view: ShaderModule<'a>,
}

impl<'a> SharcShaders<'a> {
    /// The entries, in field order.
    pub const ENTRIES: [EntryDesc; 3] = [
        EntryDesc::compute("Marlin.Engine.Lighting.SharcUpdate").push::<SharcArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.SharcResolve").push::<SharcArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.SharcView").push::<SharcArgs>(),
    ];

    /// The shaders from `modules`, in [`ENTRIES`](Self::ENTRIES) order.
    pub fn from_modules([update, resolve, view]: [ShaderModule<'a>; 3]) -> Self {
        Self {
            update,
            resolve,
            view,
        }
    }

    pub fn bind(library: &'a ShaderLibrary) -> core::result::Result<Self, BindError> {
        bind(library, &Self::ENTRIES).map(Self::from_modules)
    }
}

/// `Marlin.Engine.Lighting.SharcConstants`.
#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct Constants {
    inverse_view_projection: [f32; 16],
    eye: [f32; 4],
    sun_direction: [f32; 4],
    sun: [f32; 4],
    sky: [f32; 4],
    width: u32,
    height: u32,
    capacity: u32,
    seed: u32,
    stride: u32,
    max_vertices: u32,
    voxel_angle: f32,
    max_frames: f32,
    terminate_samples: f32,
    stale_frames: u32,
    tile_count: u32,
    tile_size: u32,
    has_lights: u32,
    candidates: u32,
    spare: [u32; 2],
}

/// `Marlin.Engine.Lighting.SharcArgs`.
#[repr(C)]
#[derive(Pod, Clone, Copy, Default)]
struct SharcArgs {
    constants: u32,
    material: u32,
    depth: u32,
    tlas: u32,
    hits: HitSources,
    simple: SimpleSources,
    lights: u32,
    tiles: u32,
    keys: u32,
    accumulation: u32,
    radiance: u32,
    ages: u32,
    cache: SharcSources,
    output: u32,
}

/// `Marlin.Engine.Lighting.SharcSources`: the cache in a reading pass's arguments.
#[repr(C)]
#[derive(Pod, Clone, Copy, Default, PartialEq, Debug)]
pub struct SharcSources {
    /// xyz: the eye the voxel sizes follow.
    pub eye: [f32; 4],
    pub keys: u32,
    pub radiance: u32,
    pub capacity: u32,
    pub voxel_angle: f32,
    pub spare: [u32; 2],
}

/// The cache after a frame's update, for passes that read it.
#[derive(Clone, Copy)]
pub struct SharcOutput<'g> {
    pub keys: BufferHandle<'g>,
    pub radiance: BufferHandle<'g>,
    capacity: u32,
    voxel_angle: f32,
    eye: [f32; 4],
    /// The frame's constants, for the view.
    constants: Constants,
}

/// A pass's views of the cache ([`SharcOutput::read`]).
#[derive(Clone, Copy)]
pub struct SharcViews<'g> {
    keys: BufferHandle<'g>,
    radiance: BufferHandle<'g>,
    capacity: u32,
    voxel_angle: f32,
    eye: [f32; 4],
}

impl<'g> SharcOutput<'g> {
    /// Declares a pass's reads of it.
    pub fn read(&self, b: &mut PassBuilder<'_, 'g>) -> SharcViews<'g> {
        SharcViews {
            keys: b.read_buffer(self.keys, Access::ComputeRead),
            radiance: b.read_buffer(self.radiance, Access::ComputeRead),
            capacity: self.capacity,
            voxel_angle: self.voxel_angle,
            eye: self.eye,
        }
    }
}

impl SharcViews<'_> {
    /// Its bindless indices and parameters, for a pass's arguments.
    pub fn bind(&self, ctx: &PassContext<'_>) -> SharcSources {
        SharcSources {
            keys: ctx.srv(self.keys),
            radiance: ctx.srv(self.radiance),
            capacity: self.capacity,
            voxel_angle: self.voxel_angle,
            spare: [0; 2],
            eye: self.eye,
        }
    }
}

/// What [`Sharc::add_passes`] reads of the frame: the main view's surfaces, where its paths
/// start, and what they hit.
#[derive(Clone, Copy)]
pub struct SharcInputs<'g> {
    /// The material buffer (`marlin_material`'s `MATERIAL_FORMAT`).
    pub material: TextureHandle<'g>,
    /// The camera's depth (`R32Float`, reverse-Z).
    pub depth: TextureHandle<'g>,
    pub rt: RtScene<'g>,
    pub simple: SimpleOutput<'g>,
    /// The GPU Scene's light table, and the frame's light tiles (`None`: no local lights).
    pub lights: BufferHandle<'g>,
    pub tiles: Option<LightTileOutput<'g>>,
}

/// The cache's pipelines and settings (see the module docs). The settings are 0010 §3's per
/// tier; the defaults are L1's.
pub struct Sharc {
    update: ComputePipeline,
    resolve: ComputePipeline,
    view: ComputePipeline,
    /// Entries: a power of two (rounded up), at least 16. 56 bytes each.
    pub capacity: u32,
    /// A path per `stride` × `stride` pixels: 4, a sixteenth of them.
    pub stride: u32,
    /// Vertices of a path, the surface it starts at included: 2 to 5.
    pub max_vertices: u32,
    /// A voxel's size over its distance to the eye (rounded down to a power of two).
    pub voxel_angle: f32,
    /// Frames' worth of samples an entry's average holds at most, at the rate paths reach it:
    /// how long it remembers.
    pub max_frames: f32,
    /// Local lights a path vertex draws from a light tile, keeping one by RIS.
    pub candidates: u32,
    /// Samples a voxel needs before paths end at it.
    pub terminate_samples: f32,
    /// Frames an entry lives without an update.
    pub stale_frames: u32,
}

impl Sharc {
    pub fn new(device: &Device, shaders: &SharcShaders<'_>) -> Result<Self> {
        let compute = |shader| {
            device.create_compute_pipeline(&ComputePipelineDesc {
                shader,
                cache: None,
            })
        };
        Ok(Self {
            update: compute(shaders.update)?,
            resolve: compute(shaders.resolve)?,
            view: compute(shaders.view)?,
            capacity: 1 << 20,
            stride: 4,
            max_vertices: 4,
            voxel_angle: 1.0 / 64.0,
            max_frames: 32.0,
            terminate_samples: 16.0,
            candidates: 16,
            stale_frames: 64,
        })
    }

    /// Declares the frame's update and resolve, the paths starting at the surfaces `inputs`
    /// holds as `camera` sees them (`width` × `height`), drawn with `seed`, lit by `env`.
    #[allow(clippy::too_many_arguments)]
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        inputs: SharcInputs<'g>,
        env: &Environment,
        (eye, camera): (Transform, Camera),
        width: u32,
        height: u32,
        seed: u32,
    ) -> SharcOutput<'g> {
        let capacity = self.capacity.max(16).next_power_of_two();
        let view_projection = crate::view_projection((eye, camera), width, height);
        let p = eye.translation;
        let sun = env.sun_illuminance / core::f32::consts::PI;
        let d = env.light_direction.normalize_or_zero();
        let [sr, sg, sb] = env.light_color;
        let [ar, ag, ab] = env.ambient;
        let stride = self.stride.max(1);
        let constants = Constants {
            inverse_view_projection: view_projection.inverse().to_cols_array(),
            eye: [p.x, p.y, p.z, 0.0],
            sun_direction: [
                d.x,
                d.y,
                d.z,
                f32::from(u8::from(sun > 0.0 && d != marlin_math::Vec3::ZERO)),
            ],
            sun: [sr * sun, sg * sun, sb * sun, 0.0],
            sky: [ar, ag, ab, 0.0],
            width,
            height,
            capacity,
            seed,
            stride,
            max_vertices: self.max_vertices.clamp(1, 5),
            voxel_angle: self.voxel_angle,
            max_frames: self.max_frames.max(1.0),
            terminate_samples: self.terminate_samples,
            stale_frames: self.stale_frames,
            tile_count: LIGHT_TILES,
            tile_size: LIGHT_TILE_SIZE,
            has_lights: u32::from(inputs.tiles.is_some()),
            candidates: self.candidates.max(1),
            spare: [0; 2],
        };
        let entries = u64::from(capacity);
        let keys = g.persistent(
            "SharcKeys",
            GLOBAL_SCOPE,
            BufferDesc::structured::<u32>(entries),
        );
        let accumulation = g.persistent(
            "SharcAccumulation",
            GLOBAL_SCOPE,
            BufferDesc::structured::<u64>(4 * entries),
        );
        let radiance = g.persistent(
            "SharcRadiance",
            GLOBAL_SCOPE,
            BufferDesc::structured::<[f32; 4]>(entries),
        );
        let ages = g.persistent(
            "SharcAges",
            GLOBAL_SCOPE,
            BufferDesc::structured::<u32>(entries),
        );
        // A new table (or a resized one) starts empty.
        let fresh = keys.fresh || accumulation.fresh || radiance.fresh || ages.fresh;
        let [mut keys, mut accumulation, mut radiance, mut ages] = [
            keys.handle,
            accumulation.handle,
            radiance.handle,
            ages.handle,
        ];
        if fresh {
            [keys, accumulation, radiance, ages] = g.add_compute("SharcClear", |b| {
                let cleared =
                    [keys, accumulation, radiance, ages].map(|h| b.write_buffer(h, Access::Clear));
                (cleared, move |ctx: &mut PassContext<'_>| {
                    for h in cleared {
                        ctx.fill_buffer(h, 0);
                    }
                })
            });
        }
        let SharcInputs {
            material,
            depth,
            rt,
            simple,
            lights,
            tiles,
        } = inputs;
        let blocks = (width.div_ceil(stride), height.div_ceil(stride));
        let (keys, accumulation) = g.add_compute("SharcUpdate", |b| {
            let material = b.read_texture(material, Access::ComputeRead);
            let depth = b.read_texture(depth, Access::ComputeRead);
            rt.read(b);
            let hits = rt.read_hits(b);
            let simple = simple.read(b);
            let lights = b.read_buffer(lights, Access::ComputeRead);
            let tiles = tiles.map(|t| b.read_buffer(t.tiles, Access::ComputeRead));
            let radiance = b.read_buffer(radiance, Access::ComputeRead);
            let keys = b.write_buffer(keys, Access::ComputeWrite);
            let accumulation = b.write_buffer(accumulation, Access::ComputeWrite);
            ((keys, accumulation), move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.update);
                let args = SharcArgs {
                    constants: ctx.upload(&constants),
                    material: ctx.srv(material),
                    depth: ctx.srv(depth),
                    tlas: rt.index,
                    hits: hits.bind(ctx),
                    simple: simple.bind(ctx),
                    lights: ctx.srv(lights),
                    // Not read without lights (`HasLights`): any buffer will do.
                    tiles: ctx.srv(tiles.unwrap_or(lights)),
                    keys: ctx.uav(keys),
                    accumulation: ctx.uav(accumulation),
                    radiance: ctx.srv(radiance),
                    ..SharcArgs::default()
                };
                ctx.push(&args);
                ctx.dispatch(blocks.0.div_ceil(8), blocks.1.div_ceil(8), 1);
            })
        });
        let (keys, radiance) = g.add_compute("SharcResolve", |b| {
            let keys = b.write_buffer(keys, Access::ComputeWrite);
            let accumulation = b.write_buffer(accumulation, Access::ComputeWrite);
            let radiance = b.write_buffer(radiance, Access::ComputeWrite);
            let ages = b.write_buffer(ages, Access::ComputeWrite);
            ((keys, radiance), move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.resolve);
                let args = SharcArgs {
                    constants: ctx.upload(&constants),
                    keys: ctx.uav(keys),
                    accumulation: ctx.uav(accumulation),
                    radiance: ctx.uav(radiance),
                    ages: ctx.uav(ages),
                    ..SharcArgs::default()
                };
                ctx.push(&args);
                ctx.dispatch(capacity.div_ceil(64), 1, 1);
            })
        });
        SharcOutput {
            keys,
            radiance,
            capacity,
            voxel_angle: self.voxel_angle,
            eye: [p.x, p.y, p.z, 0.0],
            constants,
        }
    }

    /// Declares the cache's view (`r.sharc.view`): each pixel of the frame `cache` was updated
    /// in, the radiance its surface's voxel holds (`material`, `depth` as
    /// [`add_passes`](Self::add_passes) read them), into a new [`HDR_FORMAT`] texture.
    ///
    /// [`HDR_FORMAT`]: crate::HDR_FORMAT
    pub fn add_view<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        cache: &SharcOutput<'g>,
        material: TextureHandle<'g>,
        depth: TextureHandle<'g>,
    ) -> TextureHandle<'g> {
        let constants = cache.constants;
        let (width, height) = (constants.width, constants.height);
        let output = g.create_texture(
            "SharcView",
            TextureDesc::new_2d(width, height, crate::HDR_FORMAT),
        );
        let cache = *cache;
        g.add_compute("SharcView", |b| {
            let material = b.read_texture(material, Access::ComputeRead);
            let depth = b.read_texture(depth, Access::ComputeRead);
            let views = cache.read(b);
            let output = b.write_texture(output, Access::ComputeWrite);
            (output, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.view);
                let args = SharcArgs {
                    constants: ctx.upload(&constants),
                    material: ctx.srv(material),
                    depth: ctx.srv(depth),
                    cache: views.bind(ctx),
                    output: ctx.uav(output),
                    ..SharcArgs::default()
                };
                ctx.push(&args);
                ctx.dispatch(width.div_ceil(8), height.div_ceil(8), 1);
            })
        })
    }
}
