//! The material passes of one view (0009 §5, 0011 §6), `Marlin.Engine.Material`'s: classify the
//! visibility buffer's pixels by their material's permutation, bin them, then resolve each
//! permutation's bin with one indirect dispatch, all in one graph pass so the graph's topology
//! changes only when the set of permutations does.

use alloc::string::String;
use alloc::vec::Vec;

use marlin_math::{Mat4, Transform};
use marlin_render::visibility::VisibilityOutput;
use marlin_render::{Camera, GpuScene};
use marlin_render_graph::{
    Access, BufferDesc, BufferHandle, PassContext, RenderGraph, TextureDesc, TextureHandle,
};
use marlin_rhi::{ComputePipeline, ComputePipelineDesc, Device, Format, Result, ShaderModule};
use marlin_serial::cooked::Pod;

/// The material passes' shaders (`Marlin.Engine.Material`), but for the resolves, which each
/// material type brings ([`Materials::register`]).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MaterialShaders<'a> {
    pub classify: ShaderModule<'a>,
    pub bins: ShaderModule<'a>,
    pub scatter: ShaderModule<'a>,
    pub debug: ShaderModule<'a>,
}

/// Bytes of push constants: the classify passes (11 words), a resolve (32: word 31 is the
/// indirect record's constant) and the debug view (6).
pub const CLASSIFY_PUSH_BYTES: u32 = 44;
pub const RESOLVE_PUSH_BYTES: u32 = 128;
pub const DEBUG_PUSH_BYTES: u32 = 24;

/// The material buffer's format: the dense part, 16 bytes a pixel (0011 §4.1).
pub const MATERIAL_FORMAT: Format = Format::Rgba32Uint;
/// The velocity's format: current minus last screen position, UV units.
pub const VELOCITY_FORMAT: Format = Format::Rg16Float;

/// `Marlin.Engine.Material.MaterialView`.
#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct View {
    view_projection: [f32; 16],
    previous_view_projection: [f32; 16],
    inverse_view_projection: [f32; 16],
    eye: [f32; 4],
    width: u32,
    height: u32,
    permutations: u32,
    spare: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy, Default)]
struct ClassifyArgs {
    view: u32,
    vis: u32,
    clusters: u32,
    instances: u32,
    materials: u32,
    counts: u32,
    cursors: u32,
    pixels: u32,
    args: u32,
    material: u32,
    velocity: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct ResolveArgs {
    view: u32,
    vis: u32,
    clusters: u32,
    instances: u32,
    transforms: u32,
    meshes: u32,
    materials: u32,
    params: u32,
    counts: u32,
    pixels: u32,
    material: u32,
    velocity: u32,
    permutation: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct DebugArgs {
    material: u32,
    velocity: u32,
    output: u32,
    width: u32,
    height: u32,
    mode: u32,
}

/// What [`MaterialDebug`] shows of the material buffer (0011 §11).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MaterialDebug {
    Albedo,
    Normal,
    /// Red: roughness; green: F0's luminance; blue: the fast path.
    Specular,
    /// Red and green: the velocity × 10 + 0.5.
    Velocity,
}

/// A material type's resolve permutation.
pub struct MaterialType {
    pub name: String,
    pipeline: ComputePipeline,
}

/// What [`Materials::add_passes`] makes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MaterialOutput<'g> {
    /// The visibility buffer after the material passes read it (through storage views, so
    /// its handle moves on).
    pub visbuffer: TextureHandle<'g>,
    /// [`MATERIAL_FORMAT`].
    pub material: TextureHandle<'g>,
    /// [`VELOCITY_FORMAT`].
    pub velocity: TextureHandle<'g>,
    /// Pixels per permutation, a word each.
    pub counts: BufferHandle<'g>,
}

/// The material passes and the registered material types.
pub struct Materials {
    classify: ComputePipeline,
    bins: ComputePipeline,
    scatter: ComputePipeline,
    debug: ComputePipeline,
    types: Vec<MaterialType>,
}

impl Materials {
    pub fn new(device: &Device, shaders: &MaterialShaders<'_>) -> Result<Self> {
        let compute = |shader| {
            device.create_compute_pipeline(&ComputePipelineDesc {
                shader,
                cache: None,
            })
        };
        Ok(Self {
            classify: compute(shaders.classify)?,
            bins: compute(shaders.bins)?,
            scatter: compute(shaders.scatter)?,
            debug: compute(shaders.debug)?,
            types: Vec::new(),
        })
    }

    /// Adds a material type: its resolve permutation (`Marlin.Engine.Material.ResolvePixel`
    /// instantiated with the type). Returns the permutation, the registration order: register
    /// `Marlin.Engine.DefaultMaterial` first ([`crate::DEFAULT_PERMUTATION`]).
    pub fn register(
        &mut self,
        device: &Device,
        name: &str,
        resolve: ShaderModule<'_>,
    ) -> Result<u32> {
        let pipeline = device.create_compute_pipeline(&ComputePipelineDesc {
            shader: resolve,
            cache: None,
        })?;
        self.types.push(MaterialType {
            name: name.into(),
            pipeline,
        });
        Ok(self.types.len() as u32 - 1)
    }

    pub fn types(&self) -> &[MaterialType] {
        &self.types
    }

    /// Declares the material passes of the view seen from `camera` (and, for the velocity, from
    /// `previous` the frame before; `None`: the same) over the visibility buffer `vis`, reading
    /// the GPU Scene `gpu`.
    #[allow(clippy::too_many_arguments, reason = "one view's inputs")]
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        gpu: GpuScene<'g>,
        vis: &VisibilityOutput<'g>,
        camera: (Transform, Camera),
        previous: Option<(Transform, Camera)>,
        width: u32,
        height: u32,
    ) -> MaterialOutput<'g> {
        let view_projection = |(eye, cam): (Transform, Camera)| {
            let aspect = width as f32 / height.max(1) as f32;
            Mat4::perspective_infinite_rh(cam.fov_y, aspect, cam.near)
                * eye.to_matrix().inverse_affine()
        };
        let current = view_projection(camera);
        let last = view_projection(previous.unwrap_or(camera));
        // A permutation without a parameter table has no material, so no pixels.
        let permutations = (self.types.len()).min(gpu.material_params.len()) as u32;
        let p = camera.0.translation;
        let view = View {
            view_projection: current.to_cols_array(),
            previous_view_projection: last.to_cols_array(),
            inverse_view_projection: current.inverse().to_cols_array(),
            eye: [p.x, p.y, p.z, 0.0],
            width,
            height,
            permutations,
            spare: 0,
        };
        let slots = u64::from(permutations.max(1));
        let counts = g.create_buffer("MaterialCounts", BufferDesc::structured::<u32>(slots));
        let cursors = g.create_buffer("MaterialCursors", BufferDesc::structured::<u32>(slots));
        let args = g.create_buffer("MaterialArgs", BufferDesc::structured::<[u32; 4]>(slots));
        let pixels = g.create_buffer(
            "MaterialPixels",
            BufferDesc::structured::<u32>(u64::from(width) * u64::from(height)),
        );
        let material = g.create_texture(
            "MaterialBuffer",
            TextureDesc::new_2d(width, height, MATERIAL_FORMAT),
        );
        let velocity = g.create_texture(
            "Velocity",
            TextureDesc::new_2d(width, height, VELOCITY_FORMAT),
        );
        let groups = (width.div_ceil(8), height.div_ceil(8));

        let counts = g.add_compute("MaterialReset", |b| {
            let counts = b.write_buffer(counts, Access::Clear);
            (counts, move |ctx: &mut PassContext<'_>| {
                ctx.fill_buffer(counts, 0)
            })
        });
        let (vis_tex, counts, material, velocity) = g.add_compute("MaterialClassify", |b| {
            let vis_tex = b.write_texture(vis.visbuffer, Access::ComputeWrite);
            let clusters = b.read_buffer(vis.clusters, Access::ComputeRead);
            let instances = b.read_buffer(gpu.instance_data, Access::ComputeRead);
            let materials = b.read_buffer(gpu.materials, Access::ComputeRead);
            let counts = b.write_buffer(counts, Access::ComputeWrite);
            let material = b.write_texture(material, Access::ComputeWrite);
            let velocity = b.write_texture(velocity, Access::ComputeWrite);
            (
                (vis_tex, counts, material, velocity),
                move |ctx: &mut PassContext<'_>| {
                    let view = ctx.upload(&view);
                    ctx.set_pipeline(&self.classify);
                    ctx.push(&ClassifyArgs {
                        view,
                        vis: ctx.uav(vis_tex),
                        clusters: ctx.srv(clusters),
                        instances: ctx.srv(instances),
                        materials: ctx.srv(materials),
                        counts: ctx.uav(counts),
                        material: ctx.uav(material),
                        velocity: ctx.uav(velocity),
                        ..ClassifyArgs::default()
                    });
                    ctx.dispatch(groups.0, groups.1, 1);
                },
            )
        });
        let (args, cursors) = g.add_compute("MaterialBins", |b| {
            let counts = b.read_buffer(counts, Access::ComputeRead);
            let args = b.write_buffer(args, Access::ComputeWrite);
            let cursors = b.write_buffer(cursors, Access::ComputeWrite);
            ((args, cursors), move |ctx: &mut PassContext<'_>| {
                let view = ctx.upload(&view);
                ctx.set_pipeline(&self.bins);
                ctx.push(&ClassifyArgs {
                    view,
                    counts: ctx.srv(counts),
                    cursors: ctx.uav(cursors),
                    args: ctx.uav(args),
                    ..ClassifyArgs::default()
                });
                ctx.dispatch(1, 1, 1);
            })
        });
        let (vis_tex, pixels) = g.add_compute("MaterialScatter", |b| {
            let vis_tex = b.write_texture(vis_tex, Access::ComputeWrite);
            let clusters = b.read_buffer(vis.clusters, Access::ComputeRead);
            let instances = b.read_buffer(gpu.instance_data, Access::ComputeRead);
            let materials = b.read_buffer(gpu.materials, Access::ComputeRead);
            let cursors = b.write_buffer(cursors, Access::ComputeWrite);
            let pixels = b.write_buffer(pixels, Access::ComputeWrite);
            ((vis_tex, pixels), move |ctx: &mut PassContext<'_>| {
                let view = ctx.upload(&view);
                ctx.set_pipeline(&self.scatter);
                ctx.push(&ClassifyArgs {
                    view,
                    vis: ctx.uav(vis_tex),
                    clusters: ctx.srv(clusters),
                    instances: ctx.srv(instances),
                    materials: ctx.srv(materials),
                    cursors: ctx.uav(cursors),
                    pixels: ctx.uav(pixels),
                    ..ClassifyArgs::default()
                });
                ctx.dispatch(groups.0, groups.1, 1);
            })
        });
        let records: Vec<BufferHandle<'g>> = (0..permutations)
            .map(|p| args.range(u64::from(p) * 16, 16))
            .collect();
        let params = &gpu.material_params[..permutations as usize];
        let (vis_tex, material, velocity) = g.add_compute("MaterialResolve", |b| {
            let records: Vec<BufferHandle<'g>> = records
                .iter()
                .map(|&r| b.read_buffer(r, Access::Indirect))
                .collect();
            let params: Vec<BufferHandle<'g>> = params
                .iter()
                .map(|&t| b.read_buffer(t, Access::ComputeRead))
                .collect();
            let vis_tex = b.write_texture(vis_tex, Access::ComputeWrite);
            let clusters = b.read_buffer(vis.clusters, Access::ComputeRead);
            let instances = b.read_buffer(gpu.instance_data, Access::ComputeRead);
            let transforms = b.read_buffer(gpu.instance_transforms, Access::ComputeRead);
            let meshes = b.read_buffer(gpu.meshes, Access::ComputeRead);
            let materials = b.read_buffer(gpu.materials, Access::ComputeRead);
            let counts = b.read_buffer(counts, Access::ComputeRead);
            let pixels = b.read_buffer(pixels, Access::ComputeRead);
            let material = b.write_texture(material, Access::ComputeWrite);
            let velocity = b.write_texture(velocity, Access::ComputeWrite);
            (
                (vis_tex, material, velocity),
                move |ctx: &mut PassContext<'_>| {
                    let view = ctx.upload(&view);
                    let shared = ResolveArgs {
                        view,
                        vis: ctx.uav(vis_tex),
                        clusters: ctx.srv(clusters),
                        instances: ctx.srv(instances),
                        transforms: ctx.srv(transforms),
                        meshes: ctx.srv(meshes),
                        materials: ctx.srv(materials),
                        params: 0,
                        counts: ctx.srv(counts),
                        pixels: ctx.srv(pixels),
                        material: ctx.uav(material),
                        velocity: ctx.uav(velocity),
                        permutation: 0,
                    };
                    for (p, record) in records.iter().enumerate() {
                        ctx.set_pipeline(&self.types[p].pipeline);
                        ctx.push(&ResolveArgs {
                            params: ctx.srv(params[p]),
                            permutation: p as u32,
                            ..shared
                        });
                        ctx.dispatch_indirect(*record, 1, None);
                    }
                },
            )
        });
        MaterialOutput {
            visbuffer: vis_tex,
            material,
            velocity,
            counts,
        }
    }

    /// Declares a view of `out` (0011 §11) into a new RGBA8 texture, and returns it.
    pub fn add_debug_pass<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        out: &MaterialOutput<'g>,
        mode: MaterialDebug,
    ) -> TextureHandle<'g> {
        let size = *g.texture_desc(out.material);
        let (width, height) = (size.width, size.height);
        let target = g.create_texture(
            "MaterialDebug",
            TextureDesc::new_2d(width, height, Format::Rgba8Unorm),
        );
        let (material, velocity) = (out.material, out.velocity);
        g.add_compute("MaterialDebug", |b| {
            let material = b.read_texture(material, Access::ComputeRead);
            let velocity = b.read_texture(velocity, Access::ComputeRead);
            let target = b.write_texture(target, Access::ComputeWrite);
            (target, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.debug);
                ctx.push(&DebugArgs {
                    material: ctx.srv(material),
                    velocity: ctx.srv(velocity),
                    output: ctx.uav(target),
                    width,
                    height,
                    mode: match mode {
                        MaterialDebug::Albedo => 0,
                        MaterialDebug::Normal => 1,
                        MaterialDebug::Specular => 2,
                        MaterialDebug::Velocity => 3,
                    },
                });
                ctx.dispatch(width.div_ceil(8), height.div_ceil(8), 1);
            })
        })
    }
}
