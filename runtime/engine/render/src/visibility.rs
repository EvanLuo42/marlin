//! The visibility pipeline of one view (0009 §4.1, §4.3): instance culling, the traversal of
//! every visible instance's cluster DAG hierarchy that picks its LOD cut, cluster culling, a
//! mesh shader drawing the visible clusters into a 64-bit visibility buffer with atomic max,
//! and the depth split out of it. The passes are `Marlin.Engine.Visibility`'s; its file comment
//! describes them and their buffers.
//!
//! The visibility buffer (`R64Uint`) holds, per pixel, the nearest
//! `depth << 32 | cluster << 7 | triangle`: the depth's float bits (reverse-Z, so larger is
//! nearer and 0 is empty), the index of the visible cluster in [`VisibilityOutput::clusters`]
//! (instance row, `slot << 12 | cluster`: the cluster's page slot in the page pool and its
//! index in the page) and the triangle in the cluster.
//!
//! The traversal is level by level: one indirect dispatch per level of hierarchy nodes, as
//! many as the deepest hierarchy of the drawn meshes has, rather than persistent threads
//! feeding on one queue: Vulkan does not promise forward progress between workgroups, and a
//! persistent loop that waits on another workgroup can hang the GPU.

use marlin_math::{Mat4, Transform};
use marlin_render_graph::{
    Access, BufferDesc, BufferHandle, PassContext, RenderGraph, TextureDesc, TextureHandle,
};
use marlin_rhi::{
    ComputePipeline, ComputePipelineDesc, Device, Error, ErrorKind, Format, MeshPipeline,
    MeshPipelineDesc, MultisampleState, RasterState, Result, ShaderModule,
};
use marlin_serial::cooked::Pod;

use crate::Meshes;
use crate::meshes::Camera;
use crate::scene::{GpuScene, RenderScene};

/// The visibility pipeline's shaders (`Marlin.Engine.Visibility`), which the engine gets from
/// the cooker like any asset.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VisibilityShaders<'a> {
    pub reset: ShaderModule<'a>,
    pub instance_cull: ShaderModule<'a>,
    pub node_args: ShaderModule<'a>,
    pub node_cull: ShaderModule<'a>,
    pub part_args: ShaderModule<'a>,
    pub cluster_cull: ShaderModule<'a>,
    pub finish_args: ShaderModule<'a>,
    pub mesh: ShaderModule<'a>,
    pub pixel: ShaderModule<'a>,
    pub clear: ShaderModule<'a>,
    pub depth: ShaderModule<'a>,
}

impl<'a> VisibilityShaders<'a> {
    /// The entries' names in `Marlin.Engine.Visibility`, in field order.
    pub const ENTRIES: [&'static str; 11] = [
        "Reset",
        "InstanceCull",
        "NodeArgs",
        "NodeCull",
        "PartArgs",
        "ClusterCull",
        "FinishArgs",
        "VisMesh",
        "VisPs",
        "VisClear",
        "VisDepth",
    ];

    /// The shaders `module` gives for each of [`ENTRIES`](Self::ENTRIES).
    pub fn from_fn(mut module: impl FnMut(&'static str) -> ShaderModule<'a>) -> Self {
        let [
            reset,
            instance_cull,
            node_args,
            node_cull,
            part_args,
            cluster_cull,
            finish_args,
            mesh,
            pixel,
            clear,
            depth,
        ] = Self::ENTRIES.map(&mut module);
        Self {
            reset,
            instance_cull,
            node_args,
            node_cull,
            part_args,
            cluster_cull,
            finish_args,
            mesh,
            pixel,
            clear,
            depth,
        }
    }
}

/// Bytes of push constants the cull passes set (ten 32-bit words), the raster pass (seven)
/// and the target passes (four).
pub const CULL_PUSH_BYTES: u32 = 40;
pub const RASTER_PUSH_BYTES: u32 = 28;
pub const TARGET_PUSH_BYTES: u32 = 16;

/// Fewest entries the node, part and visible-cluster buffers have: capacities round up to
/// powers of two from here, so a scene that changes a little keeps its graph plan.
const MIN_CAPACITY: u32 = 1024;
/// Most visible clusters a frame can address: the visibility buffer's 25 bits of cluster index.
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
    node_capacity: u32,
    part_capacity: u32,
    cluster_capacity: u32,
    spare: [u32; 3],
}

#[repr(C)]
#[derive(Pod, Clone, Copy, Default)]
struct CullArgs {
    view: u32,
    instances: u32,
    transforms: u32,
    bounds: u32,
    meshes: u32,
    nodes: u32,
    parts: u32,
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

/// The counters' words (see `Marlin.Engine.Visibility`).
pub mod counters {
    pub const INSTANCES: usize = 0;
    pub const NODES: usize = 1;
    pub const PARTS: usize = 2;
    /// Visible clusters found: may pass the capacity.
    pub const CLUSTERS_FOUND: usize = 3;
    pub const CLUSTERS_DRAWN: usize = 4;
    /// Words in the counters buffer.
    pub const WORDS: usize = 8;
}

/// What [`Visibility::add_passes`] makes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VisibilityOutput<'g> {
    /// `R64Uint`: see the module docs.
    pub visbuffer: TextureHandle<'g>,
    /// `R32Float`: the visibility buffer's depth (0 where nothing was drawn).
    pub depth: TextureHandle<'g>,
    /// The visible clusters: two words each (instance row, `slot << 12 | cluster`).
    pub clusters: BufferHandle<'g>,
    /// [`counters::WORDS`] words, indexed by [`counters`].
    pub counters: BufferHandle<'g>,
    /// Entries `clusters` holds.
    pub capacity: u32,
    /// Levels of hierarchy nodes traversed.
    pub levels: u32,
}

/// The visibility pipeline's GPU state: its pipelines and budgets. Needs mesh shaders and
/// 64-bit texture atomics (`Unsupported` without them).
pub struct Visibility {
    reset: ComputePipeline,
    instance_cull: ComputePipeline,
    node_args: ComputePipeline,
    node_cull: ComputePipeline,
    part_args: ComputePipeline,
    cluster_cull: ComputePipeline,
    finish_args: ComputePipeline,
    raster: MeshPipeline,
    clear: ComputePipeline,
    depth: ComputePipeline,
    /// The screen-space error, in pixels, the LOD cut may have: every instance draws the
    /// coarsest clusters whose error, projected at their distance, stays within it.
    pub lod_error_pixels: f32,
    /// Most hierarchy nodes, group parts and visible clusters a view tests and draws: the
    /// buffers are sized for the scene up to these, and what passes them is dropped (the
    /// counters still count it).
    pub max_nodes: u32,
    pub max_parts: u32,
    pub max_clusters: u32,
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

/// `needed` rounded up to a power of two within [`MIN_CAPACITY`] and `max`.
fn capacity(needed: u64, max: u32) -> u32 {
    let max = max.max(MIN_CAPACITY);
    (needed.min(u64::from(max)) as u32)
        .max(MIN_CAPACITY)
        .next_power_of_two()
        .min(max.next_power_of_two())
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
            node_args: compute(shaders.node_args)?,
            node_cull: compute(shaders.node_cull)?,
            part_args: compute(shaders.part_args)?,
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
            max_nodes: 1 << 21,
            max_parts: 1 << 21,
            max_clusters: 1 << 22,
        })
    }

    /// Declares the visibility passes of `scene` seen from `camera` into a `width` × `height`
    /// visibility buffer, reading the GPU Scene `gpu` (after its scatter). `meshes` holds the
    /// meshes: the buffers are sized for every drawable instance's whole DAG, within the budgets.
    #[allow(clippy::too_many_arguments, reason = "one view's inputs")]
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        meshes: &Meshes,
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
        let (mut nodes, mut parts, mut clusters, mut levels) = (0u64, 0u64, 0u64, 0u32);
        for (_, mesh, _) in scene.drawable() {
            let c = meshes.dag_counts(mesh);
            nodes += u64::from(c.nodes);
            parts += u64::from(c.parts);
            clusters += u64::from(c.clusters);
            levels = levels.max(c.depth);
        }
        // At least one level, so the part list is written even when nothing is drawn.
        let levels = levels.max(1);
        let node_capacity = capacity(nodes, self.max_nodes);
        let part_capacity = capacity(parts, self.max_parts);
        let cluster_capacity = capacity(clusters, self.max_clusters.min(MAX_CLUSTERS));
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
            node_capacity,
            part_capacity,
            cluster_capacity,
            spare: [0; 3],
        };

        let node_list = g.create_buffer(
            "VisNodes",
            BufferDesc::structured::<[u32; 2]>(u64::from(node_capacity)),
        );
        let part_list = g.create_buffer(
            "VisParts",
            BufferDesc::structured::<[u32; 2]>(u64::from(part_capacity)),
        );
        let counters = g.create_buffer(
            "VisCounters",
            BufferDesc::structured::<u32>(counters::WORDS as u64),
        );
        let args = g.create_buffer("VisArgs", BufferDesc::structured::<u32>(12));
        let cluster_list = g.create_buffer(
            "VisibleClusters",
            BufferDesc::structured::<[u32; 2]>(u64::from(cluster_capacity)),
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
        let mut counters = g.add_compute("VisReset", |b| {
            let counters = b.write_buffer(counters, Access::ComputeWrite);
            (counters, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.reset);
                ctx.push(&CullArgs {
                    counters: ctx.uav(counters),
                    ..CullArgs::default()
                });
                ctx.dispatch(1, 1, 1);
            })
        });
        let mut node_list = node_list;
        (node_list, counters) = g.add_compute("InstanceCull", |b| {
            let instances = b.read_buffer(gpu.instance_data, Access::ComputeRead);
            let bounds = b.read_buffer(gpu.instance_bounds, Access::ComputeRead);
            let meshes = b.read_buffer(gpu.meshes, Access::ComputeRead);
            let nodes = b.write_buffer(node_list, Access::ComputeWrite);
            let counters = b.write_buffer(counters, Access::ComputeWrite);
            ((nodes, counters), move |ctx: &mut PassContext<'_>| {
                let view = ctx.upload(&view);
                ctx.set_pipeline(&self.instance_cull);
                ctx.push(&CullArgs {
                    view,
                    instances: ctx.srv(instances),
                    bounds: ctx.srv(bounds),
                    meshes: ctx.srv(meshes),
                    nodes: ctx.uav(nodes),
                    counters: ctx.uav(counters),
                    ..CullArgs::default()
                });
                ctx.dispatch(rows.div_ceil(64), 1, 1);
            })
        });

        let mut args = args;
        let mut part_list = part_list;
        for _ in 0..levels {
            (args, counters) = g.add_compute("VisNodeArgs", |b| {
                let counters = b.write_buffer(counters, Access::ComputeWrite);
                let args = b.write_buffer(args, Access::ComputeWrite);
                ((args, counters), move |ctx: &mut PassContext<'_>| {
                    let view = ctx.upload(&view);
                    ctx.set_pipeline(&self.node_args);
                    ctx.push(&CullArgs {
                        view,
                        counters: ctx.uav(counters),
                        args: ctx.uav(args),
                        ..CullArgs::default()
                    });
                    ctx.dispatch(1, 1, 1);
                })
            });
            (node_list, part_list, counters) = g.add_compute("NodeCull", |b| {
                let dispatch = b.read_buffer(args.range(0, 16), Access::Indirect);
                let instances = b.read_buffer(gpu.instance_data, Access::ComputeRead);
                let transforms = b.read_buffer(gpu.instance_transforms, Access::ComputeRead);
                let meshes = b.read_buffer(gpu.meshes, Access::ComputeRead);
                let nodes = b.write_buffer(node_list, Access::ComputeWrite);
                let parts = b.write_buffer(part_list, Access::ComputeWrite);
                let counters = b.write_buffer(counters, Access::ComputeWrite);
                (
                    (nodes, parts, counters),
                    move |ctx: &mut PassContext<'_>| {
                        let view = ctx.upload(&view);
                        ctx.set_pipeline(&self.node_cull);
                        ctx.push(&CullArgs {
                            view,
                            instances: ctx.srv(instances),
                            transforms: ctx.srv(transforms),
                            meshes: ctx.srv(meshes),
                            nodes: ctx.uav(nodes),
                            parts: ctx.uav(parts),
                            counters: ctx.uav(counters),
                            ..CullArgs::default()
                        });
                        ctx.dispatch_indirect(dispatch, 1, None);
                    },
                )
            });
        }

        let args = g.add_compute("VisPartArgs", |b| {
            let counters = b.read_buffer(counters, Access::ComputeRead);
            let args = b.write_buffer(args, Access::ComputeWrite);
            (args, move |ctx: &mut PassContext<'_>| {
                let view = ctx.upload(&view);
                ctx.set_pipeline(&self.part_args);
                ctx.push(&CullArgs {
                    view,
                    counters: ctx.srv(counters),
                    args: ctx.uav(args),
                    ..CullArgs::default()
                });
                ctx.dispatch(1, 1, 1);
            })
        });
        let (cluster_list, counters) = g.add_compute("ClusterCull", |b| {
            let dispatch = b.read_buffer(args.range(16, 16), Access::Indirect);
            let instances = b.read_buffer(gpu.instance_data, Access::ComputeRead);
            let transforms = b.read_buffer(gpu.instance_transforms, Access::ComputeRead);
            let meshes = b.read_buffer(gpu.meshes, Access::ComputeRead);
            let parts = b.read_buffer(part_list, Access::ComputeRead);
            let counters = b.write_buffer(counters, Access::ComputeWrite);
            let clusters = b.write_buffer(cluster_list, Access::ComputeWrite);
            ((clusters, counters), move |ctx: &mut PassContext<'_>| {
                let view = ctx.upload(&view);
                ctx.set_pipeline(&self.cluster_cull);
                ctx.push(&CullArgs {
                    view,
                    instances: ctx.srv(instances),
                    transforms: ctx.srv(transforms),
                    meshes: ctx.srv(meshes),
                    parts: ctx.srv(parts),
                    counters: ctx.uav(counters),
                    clusters: ctx.uav(clusters),
                    ..CullArgs::default()
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
                    ..CullArgs::default()
                });
                ctx.dispatch(1, 1, 1);
            })
        });
        let vis = g.add_raster("VisRaster", |b| {
            b.area(width, height);
            let dispatch = b.read_buffer(args.range(32, 16), Access::Indirect);
            let instances = b.read_buffer(gpu.instance_data, Access::MeshRead);
            let transforms = b.read_buffer(gpu.instance_transforms, Access::MeshRead);
            let meshes = b.read_buffer(gpu.meshes, Access::MeshRead);
            let clusters = b.read_buffer(cluster_list, Access::MeshRead);
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
            clusters: cluster_list,
            counters,
            capacity: cluster_capacity,
            levels,
        }
    }
}
