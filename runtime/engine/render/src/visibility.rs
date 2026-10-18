//! The visibility pipeline of one view (0009 §4.1, §4.3), R1's discrete-LOD form: instance and
//! cluster frustum culling on the GPU, a mesh shader drawing the visible clusters into a 64-bit
//! visibility buffer with atomic max, and the depth split out of it. The passes are
//! `Marlin.Engine.Visibility`'s; its file comment describes them and their buffers.
//!
//! The visibility buffer (`R64Uint`) holds, per pixel, the nearest
//! `depth << 32 | cluster << 7 | triangle`: the depth's float bits (reverse-Z, so larger is
//! nearer and 0 is empty), the index of the visible cluster in [`VisibilityOutput::clusters`]
//! (instance row, meshlet | LOD << 24) and the triangle in the meshlet.

use marlin_math::{Mat4, Transform};
use marlin_render_graph::{
    Access, BufferDesc, BufferHandle, PassContext, RenderGraph, TextureDesc, TextureHandle,
};
use marlin_rhi::{
    ComputePipeline, ComputePipelineDesc, Device, Error, ErrorKind, Format, MeshPipeline,
    MeshPipelineDesc, MultisampleState, RasterState, Result, ShaderModule,
};
use marlin_serial::cooked::Pod;

use crate::Renderer;
use crate::renderer::Camera;
use crate::scene::{GpuScene, RenderScene};

/// The visibility pipeline's shaders (`Marlin.Engine.Visibility`), which the engine gets from
/// the cooker like any asset.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VisibilityShaders<'a> {
    pub reset: ShaderModule<'a>,
    pub instance_cull: ShaderModule<'a>,
    pub instance_args: ShaderModule<'a>,
    pub cluster_cull: ShaderModule<'a>,
    pub finish_args: ShaderModule<'a>,
    pub mesh: ShaderModule<'a>,
    pub pixel: ShaderModule<'a>,
    pub clear: ShaderModule<'a>,
    pub depth: ShaderModule<'a>,
}

/// Bytes of push constants the cull passes set (nine 32-bit words), the raster pass (seven)
/// and the target passes (four).
pub const CULL_PUSH_BYTES: u32 = 36;
pub const RASTER_PUSH_BYTES: u32 = 28;
pub const TARGET_PUSH_BYTES: u32 = 16;

/// Fewest entries the visible-cluster buffer has: capacities round up to powers of two from
/// here, so a scene that changes a little keeps its graph plan.
const MIN_CLUSTER_CAPACITY: u32 = 1024;
/// Most visible clusters a frame: the visibility buffer's 25 bits of cluster index.
pub const MAX_CLUSTERS: u32 = 1 << 25;

/// `Marlin.Engine.Visibility.View`.
#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct View {
    view_projection: [f32; 16],
    planes: [[f32; 4]; 5],
    eye: [f32; 4],
    lod_error_pixels: f32,
    instance_rows: u32,
    cluster_capacity: u32,
    spare: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct CullArgs {
    view: u32,
    instances: u32,
    transforms: u32,
    bounds: u32,
    meshes: u32,
    visible: u32,
    counters: u32,
    args: u32,
    clusters: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct RasterArgs {
    view: u32,
    instances: u32,
    transforms: u32,
    meshes: u32,
    clusters: u32,
    counters: u32,
    vis: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct TargetArgs {
    vis: u32,
    depth: u32,
    width: u32,
    height: u32,
}

/// What [`Visibility::add_passes`] makes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VisibilityOutput<'g> {
    /// `R64Uint`: see the module docs.
    pub visbuffer: TextureHandle<'g>,
    /// `R32Float`: the visibility buffer's depth (0 where nothing was drawn).
    pub depth: TextureHandle<'g>,
    /// The visible clusters: two words each (instance row, meshlet | LOD << 24).
    pub clusters: BufferHandle<'g>,
    /// Three words: visible instances, visible clusters found (may pass the capacity),
    /// clusters drawn.
    pub counters: BufferHandle<'g>,
    /// Entries `clusters` holds.
    pub capacity: u32,
}

/// The visibility pipeline's GPU state: its pipelines. Needs mesh shaders and 64-bit texture
/// atomics (`Unsupported` without them).
pub struct Visibility {
    reset: ComputePipeline,
    instance_cull: ComputePipeline,
    instance_args: ComputePipeline,
    cluster_cull: ComputePipeline,
    finish_args: ComputePipeline,
    raster: MeshPipeline,
    clear: ComputePipeline,
    depth: ComputePipeline,
    /// The screen-space error, in pixels, a LOD may have (as [`Renderer::lod_error_pixels`]).
    pub lod_error_pixels: f32,
}

/// World-space frustum planes of a view-projection (reverse-Z, infinite far): left, right,
/// bottom, top, near, each normalized so `dot(xyz, p) + w` is a distance.
fn planes(view_projection: &Mat4) -> [[f32; 4]; 5] {
    let m = view_projection.to_cols_array();
    let row = |r: usize| [m[r], m[4 + r], m[8 + r], m[12 + r]];
    let (r0, r1, r2, r3) = (row(0), row(1), row(2), row(3));
    let add = |a: [f32; 4], b: [f32; 4], s: f32| core::array::from_fn(|i| a[i] + s * b[i]);
    // Reverse-Z: the near plane is z = w.
    let raw: [[f32; 4]; 5] = [
        add(r3, r0, 1.0),
        add(r3, r0, -1.0),
        add(r3, r1, 1.0),
        add(r3, r1, -1.0),
        add(r3, r2, -1.0),
    ];
    raw.map(|p| {
        let l = marlin_math::scalar::sqrt(p[0] * p[0] + p[1] * p[1] + p[2] * p[2]);
        p.map(|c| c / l)
    })
}

impl Visibility {
    pub fn new(device: &Device, shaders: &VisibilityShaders<'_>) -> Result<Self> {
        let f = device.features();
        if !f.mesh_shader || !f.texture_atomic_int64 {
            return Err(Error::from_kind(ErrorKind::Unsupported));
        }
        let compute = |shader| {
            device.create_compute_pipeline(&ComputePipelineDesc {
                shader,
                cache: None,
            })
        };
        Ok(Self {
            reset: compute(shaders.reset)?,
            instance_cull: compute(shaders.instance_cull)?,
            instance_args: compute(shaders.instance_args)?,
            cluster_cull: compute(shaders.cluster_cull)?,
            finish_args: compute(shaders.finish_args)?,
            raster: device.create_mesh_pipeline(&MeshPipelineDesc {
                amplification: None,
                mesh: shaders.mesh,
                pixel: Some(shaders.pixel),
                raster: RasterState::default(),
                depth: None,
                color_targets: &[],
                multisample: MultisampleState::NONE,
                cache: None,
            })?,
            clear: compute(shaders.clear)?,
            depth: compute(shaders.depth)?,
            lod_error_pixels: 1.0,
        })
    }

    /// Declares the visibility passes of `scene` seen from `camera` into a `width` × `height`
    /// visibility buffer, reading the GPU Scene `gpu` (after its scatter). `renderer` holds the
    /// meshes: the visible-cluster buffer is sized for every drawable instance's largest LOD.
    #[allow(clippy::too_many_arguments, reason = "one view's inputs")]
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        renderer: &Renderer,
        scene: &RenderScene,
        gpu: GpuScene<'g>,
        camera: (Transform, Camera),
        width: u32,
        height: u32,
    ) -> VisibilityOutput<'g> {
        let (eye, cam) = camera;
        let aspect = width as f32 / height.max(1) as f32;
        let view_projection = Mat4::perspective_infinite_rh(cam.fov_y, aspect, cam.near)
            * eye.to_matrix().inverse_affine();
        let needed: u64 = scene
            .drawable()
            .map(|(_, mesh, _)| u64::from(renderer.meshlet_capacity(mesh)))
            .sum();
        let capacity = (needed.min(u64::from(MAX_CLUSTERS)) as u32)
            .max(MIN_CLUSTER_CAPACITY)
            .next_power_of_two();
        let rows = scene.instance_rows();
        let p = eye.translation;
        let view = View {
            view_projection: view_projection.to_cols_array(),
            planes: planes(&view_projection),
            eye: [
                p.x,
                p.y,
                p.z,
                height as f32 / (2.0 * marlin_math::scalar::tan(cam.fov_y * 0.5)),
            ],
            lod_error_pixels: self.lod_error_pixels,
            instance_rows: rows,
            cluster_capacity: capacity,
            spare: 0,
        };

        let visible = g.create_buffer(
            "VisibleInstances",
            BufferDesc::structured::<[u32; 4]>(u64::from(rows.max(1))),
        );
        let counters = g.create_buffer("VisCounters", BufferDesc::structured::<u32>(4));
        let args = g.create_buffer("VisArgs", BufferDesc::structured::<u32>(8));
        let clusters = g.create_buffer(
            "VisibleClusters",
            BufferDesc::structured::<[u32; 2]>(u64::from(capacity)),
        );
        let vis = g.create_texture(
            "VisBuffer",
            TextureDesc::new_2d(width, height, Format::R64Uint),
        );
        let depth = g.create_texture(
            "VisDepth",
            TextureDesc::new_2d(width, height, Format::R32Float),
        );
        let groups = (width.div_ceil(8), height.div_ceil(8));

        let vis = g.add_compute("VisClear", |b| {
            let vis = b.write_texture(vis, Access::ComputeWrite);
            (vis, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.clear);
                ctx.push(&TargetArgs {
                    vis: ctx.uav(vis),
                    depth: 0,
                    width,
                    height,
                });
                ctx.dispatch(groups.0, groups.1, 1);
            })
        });
        let counters = g.add_compute("VisReset", |b| {
            let counters = b.write_buffer(counters, Access::ComputeWrite);
            (counters, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.reset);
                ctx.push(&CullArgs {
                    counters: ctx.uav(counters),
                    ..CullArgs::zeroed()
                });
                ctx.dispatch(1, 1, 1);
            })
        });
        let (visible, counters) = g.add_compute("InstanceCull", |b| {
            let instances = b.read_buffer(gpu.instance_data, Access::ComputeRead);
            let transforms = b.read_buffer(gpu.instance_transforms, Access::ComputeRead);
            let bounds = b.read_buffer(gpu.instance_bounds, Access::ComputeRead);
            let meshes = b.read_buffer(gpu.meshes, Access::ComputeRead);
            let visible = b.write_buffer(visible, Access::ComputeWrite);
            let counters = b.write_buffer(counters, Access::ComputeWrite);
            ((visible, counters), move |ctx: &mut PassContext<'_>| {
                let view = ctx.upload(&view);
                ctx.set_pipeline(&self.instance_cull);
                ctx.push(&CullArgs {
                    view,
                    instances: ctx.srv(instances),
                    transforms: ctx.srv(transforms),
                    bounds: ctx.srv(bounds),
                    meshes: ctx.srv(meshes),
                    visible: ctx.uav(visible),
                    counters: ctx.uav(counters),
                    args: 0,
                    clusters: 0,
                });
                ctx.dispatch(rows.div_ceil(64), 1, 1);
            })
        });
        let args = g.add_compute("VisInstanceArgs", |b| {
            let counters = b.read_buffer(counters, Access::ComputeRead);
            let args = b.write_buffer(args, Access::ComputeWrite);
            (args, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.instance_args);
                ctx.push(&CullArgs {
                    counters: ctx.srv(counters),
                    args: ctx.uav(args),
                    ..CullArgs::zeroed()
                });
                ctx.dispatch(1, 1, 1);
            })
        });
        let (clusters, counters) = g.add_compute("ClusterCull", |b| {
            let dispatch = b.read_buffer(args.range(0, 16), Access::Indirect);
            let instances = b.read_buffer(gpu.instance_data, Access::ComputeRead);
            let transforms = b.read_buffer(gpu.instance_transforms, Access::ComputeRead);
            let meshes = b.read_buffer(gpu.meshes, Access::ComputeRead);
            let visible = b.read_buffer(visible, Access::ComputeRead);
            let counters = b.write_buffer(counters, Access::ComputeWrite);
            let clusters = b.write_buffer(clusters, Access::ComputeWrite);
            ((clusters, counters), move |ctx: &mut PassContext<'_>| {
                let view = ctx.upload(&view);
                ctx.set_pipeline(&self.cluster_cull);
                ctx.push(&CullArgs {
                    view,
                    instances: ctx.srv(instances),
                    transforms: ctx.srv(transforms),
                    bounds: 0,
                    meshes: ctx.srv(meshes),
                    visible: ctx.srv(visible),
                    counters: ctx.uav(counters),
                    args: 0,
                    clusters: ctx.uav(clusters),
                });
                ctx.dispatch_indirect(dispatch, 1, None);
            })
        });
        let (args, counters) = g.add_compute("VisMeshArgs", |b| {
            let counters = b.write_buffer(counters, Access::ComputeWrite);
            let args = b.write_buffer(args, Access::ComputeWrite);
            ((args, counters), move |ctx: &mut PassContext<'_>| {
                let view = ctx.upload(&view);
                ctx.set_pipeline(&self.finish_args);
                ctx.push(&CullArgs {
                    view,
                    counters: ctx.uav(counters),
                    args: ctx.uav(args),
                    ..CullArgs::zeroed()
                });
                ctx.dispatch(1, 1, 1);
            })
        });
        let vis = g.add_raster("VisRaster", |b| {
            b.area(width, height);
            let dispatch = b.read_buffer(args.range(16, 16), Access::Indirect);
            let instances = b.read_buffer(gpu.instance_data, Access::MeshRead);
            let transforms = b.read_buffer(gpu.instance_transforms, Access::MeshRead);
            let meshes = b.read_buffer(gpu.meshes, Access::MeshRead);
            let clusters = b.read_buffer(clusters, Access::MeshRead);
            let counters = b.read_buffer(counters, Access::MeshRead);
            let vis = b.write_texture(vis, Access::PixelWrite);
            (vis, move |ctx: &mut PassContext<'_>| {
                let view = ctx.upload(&view);
                ctx.set_pipeline(&self.raster);
                ctx.push(&RasterArgs {
                    view,
                    instances: ctx.srv(instances),
                    transforms: ctx.srv(transforms),
                    meshes: ctx.srv(meshes),
                    clusters: ctx.srv(clusters),
                    counters: ctx.srv(counters),
                    vis: ctx.uav(vis),
                });
                ctx.dispatch_mesh_indirect(dispatch, 1, None);
            })
        });
        let (vis, depth) = g.add_compute("VisDepth", |b| {
            let vis = b.write_texture(vis, Access::ComputeWrite);
            let depth = b.write_texture(depth, Access::ComputeWrite);
            ((vis, depth), move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.depth);
                ctx.push(&TargetArgs {
                    vis: ctx.uav(vis),
                    depth: ctx.uav(depth),
                    width,
                    height,
                });
                ctx.dispatch(groups.0, groups.1, 1);
            })
        });
        VisibilityOutput {
            visbuffer: vis,
            depth,
            clusters,
            counters,
            capacity,
        }
    }
}

impl CullArgs {
    const fn zeroed() -> Self {
        Self {
            view: 0,
            instances: 0,
            transforms: 0,
            bounds: 0,
            meshes: 0,
            visible: 0,
            counters: 0,
            args: 0,
            clusters: 0,
        }
    }
}
