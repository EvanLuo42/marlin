//! The visibility pipeline of one view (0009 §4.1–§4.3): instance culling, the traversal of
//! every visible instance's cluster DAG hierarchy that picks its LOD cut, cluster culling, a
//! mesh shader drawing the visible clusters into a 64-bit visibility buffer with atomic max,
//! and the depth split out of it; twice, with two-pass occlusion culling against a
//! hierarchical depth buffer (HZB). The passes are `Marlin.Engine.Visibility`'s; its file
//! comment describes them and their buffers.
//!
//! The visibility buffer (`R64Uint`) holds, per pixel, the nearest
//! `depth << 32 | cluster << 7 | triangle`: the depth's float bits (reverse-Z, so larger is
//! nearer and 0 is empty), the index of the visible cluster in [`VisibilityOutput::clusters`]
//! (instance row, `slot << 12 | cluster`: the cluster's page slot in the page pool and its
//! index in the page) and the triangle in the cluster.
//!
//! **Occlusion** (§4.2): pass 1 culls against the HZB the last frame left in the view's history
//! (with the last frame's view-projection) and keeps what it culls; an HZB is built from what
//! pass 1 drew; pass 2 tests what pass 1 culled against it and draws what is visible after all;
//! the final HZB, built from both, is the next frame's. Without a last frame (the first frame,
//! a resize, a camera cut: no previous camera) pass 1 culls nothing by occlusion and there is no
//! pass 2.
//!
//! The traversal is level by level: one indirect dispatch per level of hierarchy nodes, as
//! many as the deepest hierarchy of the drawn meshes has, rather than persistent threads
//! feeding on one queue: Vulkan does not promise forward progress between workgroups, and a
//! persistent loop that waits on another workgroup can hang the GPU.

use alloc::vec::Vec;

use marlin_math::{Mat4, Transform};
use marlin_render_graph::{
    Access, BufferDesc, BufferHandle, PassContext, RenderGraph, TextureDesc, TextureHandle,
};
use marlin_rhi::{
    ComputePipeline, ComputePipelineDesc, CullMode, Device, Error, ErrorKind, Format, MeshPipeline,
    MeshPipelineDesc, MultisampleState, RasterState, Result, ShaderModule,
};
use marlin_serial::cooked::Pod;
use marlin_shader::asset::ShaderLibrary;
use marlin_shader::bind::{BindError, EntryDesc, bind};
use marlin_shader::reflect::Stage;

use crate::Meshes;
use crate::meshes::{Camera, PagePool};
use crate::scene::{GpuScene, RenderScene};

/// The visibility pipeline's shaders (`Marlin.Engine.Visibility`), which the engine gets from
/// the cooker like any asset.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VisibilityShaders<'a> {
    pub reset: ShaderModule<'a>,
    pub instance_cull: ShaderModule<'a>,
    pub instance_retest_args: ShaderModule<'a>,
    pub instance_retest: ShaderModule<'a>,
    pub node_args: ShaderModule<'a>,
    pub node_cull: ShaderModule<'a>,
    pub part_args: ShaderModule<'a>,
    pub cluster_cull: ShaderModule<'a>,
    pub cluster_retest_args: ShaderModule<'a>,
    pub cluster_retest: ShaderModule<'a>,
    pub finish_args: ShaderModule<'a>,
    pub mesh: ShaderModule<'a>,
    pub pixel: ShaderModule<'a>,
    pub clear: ShaderModule<'a>,
    pub depth: ShaderModule<'a>,
    pub hzb_first: ShaderModule<'a>,
    pub hzb_down: ShaderModule<'a>,
    pub sw_raster: ShaderModule<'a>,
    pub shadow_mesh: ShaderModule<'a>,
    pub shadow_pixel: ShaderModule<'a>,
}

impl<'a> VisibilityShaders<'a> {
    /// The entries, in field order.
    pub const ENTRIES: [EntryDesc; 20] = [
        cull("Marlin.Engine.Visibility.Reset"),
        cull("Marlin.Engine.Visibility.InstanceCull"),
        cull("Marlin.Engine.Visibility.InstanceRetestArgs"),
        cull("Marlin.Engine.Visibility.InstanceRetest"),
        cull("Marlin.Engine.Visibility.NodeArgs"),
        cull("Marlin.Engine.Visibility.NodeCull"),
        cull("Marlin.Engine.Visibility.PartArgs"),
        cull("Marlin.Engine.Visibility.ClusterCull"),
        cull("Marlin.Engine.Visibility.ClusterRetestArgs"),
        cull("Marlin.Engine.Visibility.ClusterRetest"),
        cull("Marlin.Engine.Visibility.FinishArgs"),
        EntryDesc::new("Marlin.Engine.Visibility.VisMesh", Stage::Mesh).push::<RasterArgs>(),
        EntryDesc::new("Marlin.Engine.Visibility.VisPs", Stage::Pixel).push::<RasterArgs>(),
        EntryDesc::compute("Marlin.Engine.Visibility.VisClear").push::<TargetArgs>(),
        EntryDesc::compute("Marlin.Engine.Visibility.VisDepth").push::<TargetArgs>(),
        EntryDesc::compute("Marlin.Engine.Visibility.HzbFirst").push::<HzbArgs>(),
        EntryDesc::compute("Marlin.Engine.Visibility.HzbDown").push::<HzbArgs>(),
        EntryDesc::compute("Marlin.Engine.Visibility.SwRaster").push::<RasterArgs>(),
        EntryDesc::new("Marlin.Engine.Visibility.ShadowMesh", Stage::Mesh).push::<RasterArgs>(),
        EntryDesc::new("Marlin.Engine.Visibility.ShadowPs", Stage::Pixel).push::<RasterArgs>(),
    ];

    /// The shaders from `modules`, in [`ENTRIES`](Self::ENTRIES) order.
    pub fn from_modules(
        [
            reset,
            instance_cull,
            instance_retest_args,
            instance_retest,
            node_args,
            node_cull,
            part_args,
            cluster_cull,
            cluster_retest_args,
            cluster_retest,
            finish_args,
            mesh,
            pixel,
            clear,
            depth,
            hzb_first,
            hzb_down,
            sw_raster,
            shadow_mesh,
            shadow_pixel,
        ]: [ShaderModule<'a>; 20],
    ) -> Self {
        Self {
            reset,
            instance_cull,
            instance_retest_args,
            instance_retest,
            node_args,
            node_cull,
            part_args,
            cluster_cull,
            cluster_retest_args,
            cluster_retest,
            finish_args,
            mesh,
            pixel,
            clear,
            depth,
            hzb_first,
            hzb_down,
            sw_raster,
            shadow_mesh,
            shadow_pixel,
        }
    }

    /// The visibility pipeline's shaders from `library`, checked against what its passes set.
    pub fn bind(library: &'a ShaderLibrary) -> core::result::Result<Self, BindError> {
        bind(library, &Self::ENTRIES).map(Self::from_modules)
    }
}

/// A cull pass's entry: compute, setting [`CullArgs`].
const fn cull(name: &'static str) -> EntryDesc {
    EntryDesc::compute(name).push::<CullArgs>()
}

/// Fewest entries the node, part, occluded and visible-cluster buffers have: capacities round
/// up to powers of two from here, so a scene that changes a little keeps its graph plan.
const MIN_CAPACITY: u32 = 1024;
/// Most visible clusters a frame can address: the visibility buffer's 25 bits of cluster index.
pub const MAX_CLUSTERS: u32 = 1 << 25;

/// `CullArgs.Pass`: test against the HZB; the second pass.
const TEST_HZB: u32 = 1;
const SECOND_PASS: u32 = 2;
/// `View.Mode`: an orthographic shadow view.
const ORTHOGRAPHIC: u32 = 1;

/// The HZB history's name.
const HZB: &str = "VisHzb";

/// `Marlin.Engine.Visibility.View`.
#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct View {
    view_projection: [f32; 16],
    previous_view_projection: [f32; 16],
    planes: [[f32; 4]; 6],
    eye: [f32; 4],
    lod_error_pixels: f32,
    instance_rows: u32,
    node_capacity: u32,
    part_capacity: u32,
    cluster_capacity: u32,
    request_capacity: u32,
    occluded_capacity: u32,
    hzb_mips: u32,
    hzb_size: [u32; 2],
    sw_edge_pixels: f32,
    width: u32,
    height: u32,
    mode: u32,
    shadow_pages: u32,
    shadow_pool: u32,
    shadow_page_base: u32,
    shadow_pages_per_side: u32,
    shadow_page_size: u32,
    shadow_pool_pages_per_row: u32,
    shadow_page_offset: [u32; 2],
    shadow_drawn_pages: u32,
    shadow_drawn_base: u32,
    shadow_draw_capacity: u32,
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
    requests: u32,
    used: u32,
    hzb: u32,
    occluded_instances: u32,
    occluded_clusters: u32,
    shadow_draws: u32,
    pass: u32,
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
    shadow_draws: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct TargetArgs {
    vis: u32,
    depth: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct HzbArgs {
    source: u32,
    output: u32,
    source_width: u32,
    source_height: u32,
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
    /// Page requests made: may pass the capacity.
    pub const REQUESTS: usize = 8;
    /// Instances and clusters pass 1 found occluded.
    pub const OCCLUDED_INSTANCES: usize = 9;
    /// The first cluster of the last pass: pass 1 drew the clusters before it.
    pub const PASS_FIRST_CLUSTER: usize = 10;
    pub const OCCLUDED_CLUSTERS: usize = 11;
    /// Software-raster clusters found (may pass the capacity) and drawn, and the first of the
    /// last pass.
    pub const SW_CLUSTERS_FOUND: usize = 15;
    pub const SW_CLUSTERS_DRAWN: usize = 16;
    pub const PASS_FIRST_SW_CLUSTER: usize = 17;
    /// Words in the counters buffer.
    pub const WORDS: usize = 20;
}

/// One view the visibility passes draw.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ViewDesc {
    /// Which view: its HZB is kept in the graph's history under it.
    pub id: u64,
    pub camera: (Transform, Camera),
    /// The last frame's camera of the view, whose HZB this frame's first pass tests against;
    /// `None` after a camera cut (or on the first frame).
    pub previous: Option<(Transform, Camera)>,
    pub width: u32,
    pub height: u32,
}

/// What [`Visibility::add_passes`] makes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VisibilityOutput<'g> {
    /// `R64Uint`: see the module docs.
    pub visbuffer: TextureHandle<'g>,
    /// `R32Float`: the visibility buffer's depth (0 where nothing was drawn).
    pub depth: TextureHandle<'g>,
    /// The visible clusters: two words each (instance row, `slot << 12 | cluster`), the
    /// hardware raster's from 0, the software raster's from `capacity`.
    pub clusters: BufferHandle<'g>,
    /// [`counters::WORDS`] words, indexed by [`counters`].
    pub counters: BufferHandle<'g>,
    /// Entries each half of `clusters` holds: hardware-raster clusters are in the first,
    /// software-raster ones from `capacity` on.
    pub capacity: u32,
    /// Levels of hierarchy nodes traversed.
    pub levels: u32,
    /// The page pool, as the passes read it: what later passes reading clusters declare.
    pub pages: BufferHandle<'g>,
    /// The page requests (two words each: the byte offset of the page's table entry in the
    /// pool, the priority as f32 bits) and the used-slot marks: the streaming feedback
    /// (`Meshes::apply_feedback`).
    pub requests: BufferHandle<'g>,
    pub used: BufferHandle<'g>,
    /// Entries `requests` holds.
    pub request_capacity: u32,
    /// The final HZB (`R32Float`, all mips; the next frame's), if occlusion culling is on.
    pub hzb: Option<TextureHandle<'g>>,
    /// Whether pass 1 tested against the last frame's HZB (and pass 2 ran).
    pub two_pass: bool,
}

/// The visibility pipeline's GPU state: its pipelines and budgets. Needs mesh shaders and
/// 64-bit texture atomics (`Unsupported` without them).
pub struct Visibility {
    reset: ComputePipeline,
    instance_cull: ComputePipeline,
    instance_retest_args: ComputePipeline,
    instance_retest: ComputePipeline,
    node_args: ComputePipeline,
    node_cull: ComputePipeline,
    part_args: ComputePipeline,
    cluster_cull: ComputePipeline,
    cluster_retest_args: ComputePipeline,
    cluster_retest: ComputePipeline,
    finish_args: ComputePipeline,
    raster: MeshPipeline,
    /// `ShadowMesh` + `ShadowPs`, both sides of every triangle.
    shadow_raster: MeshPipeline,
    clear: ComputePipeline,
    depth: ComputePipeline,
    hzb_first: ComputePipeline,
    hzb_down: ComputePipeline,
    sw_raster: ComputePipeline,
    /// The screen-space error, in pixels, the LOD cut may have: every instance draws the
    /// coarsest clusters whose error, projected at their distance, stays within it.
    pub lod_error_pixels: f32,
    /// Two-pass occlusion culling (on by default).
    pub occlusion: bool,
    /// Most hierarchy nodes, group parts, visible clusters and occluded instances or clusters
    /// a view tests and draws: the buffers are sized for the scene up to these, and what passes
    /// them is dropped (the counters still count it; occluded ones that do not fit are drawn).
    pub max_nodes: u32,
    pub max_parts: u32,
    pub max_clusters: u32,
    pub max_occluded: u32,
    /// Most page requests a view makes a frame.
    pub max_requests: u32,
    /// Most draws (a cluster over a page) a shadow view makes a frame.
    pub max_shadow_draws: u32,
    /// Clusters whose triangles' average edge on screen is shorter than this many pixels
    /// draw in software (`r.geo.sw_raster_edge_px`, 0009 §4.1): 0 draws everything with the
    /// mesh shader.
    pub sw_edge_pixels: f32,
}

/// World-space frustum planes of a reverse-Z view-projection: left, right, bottom, top, near
/// (z ≤ w), far (z ≥ 0), each normalized so `dot(xyz, p) + w` is a distance. A plane with no
/// direction (an infinite far plane) is `(0, 0, 0, 1)`: everything is inside.
pub(crate) fn planes(view_projection: &Mat4) -> [[f32; 4]; 6] {
    let m = view_projection.to_cols_array();
    let row = |r: usize| [m[r], m[4 + r], m[8 + r], m[12 + r]];
    let (r0, r1, r2, r3) = (row(0), row(1), row(2), row(3));
    let add = |a: [f32; 4], b: [f32; 4], s: f32| core::array::from_fn(|i| a[i] + s * b[i]);
    // Reverse-Z: the near plane is z = w.
    let raw: [[f32; 4]; 6] = [
        add(r3, r0, 1.0),
        add(r3, r0, -1.0),
        add(r3, r1, 1.0),
        add(r3, r1, -1.0),
        add(r3, r2, -1.0),
        r2,
    ];
    raw.map(|p| {
        let l = marlin_math::scalar::sqrt(p[0] * p[0] + p[1] * p[1] + p[2] * p[2]);
        if l < 1e-20 {
            [0.0, 0.0, 0.0, 1.0]
        } else {
            p.map(|c| c / l)
        }
    })
}

/// `camera`'s view-projection for a `width` × `height` view.
fn view_projection((eye, camera): (Transform, Camera), width: u32, height: u32) -> Mat4 {
    let aspect = width as f32 / height.max(1) as f32;
    Mat4::perspective_infinite_rh(camera.fov_y, aspect, camera.near)
        * eye.to_matrix().inverse_affine()
}

/// `needed` rounded up to a power of two within [`MIN_CAPACITY`] and `max`.
fn capacity(needed: u64, max: u32) -> u32 {
    let max = max.max(MIN_CAPACITY);
    (needed.min(u64::from(max)) as u32)
        .max(MIN_CAPACITY)
        .next_power_of_two()
        .min(max.next_power_of_two())
}

/// The HZB's mip-0 size for a `width` × `height` view: a power of two per axis, at least half
/// the view; and its mip count.
pub fn hzb_size(width: u32, height: u32) -> ([u32; 2], u32) {
    let w = (width.max(1).next_power_of_two() / 2).max(1);
    let h = (height.max(1).next_power_of_two() / 2).max(1);
    ([w, h], w.max(h).ilog2() + 1)
}

/// The buffers the cull passes thread through, at their latest versions.
#[derive(Clone, Copy)]
struct Lists<'g> {
    nodes: BufferHandle<'g>,
    parts: BufferHandle<'g>,
    counters: BufferHandle<'g>,
    args: BufferHandle<'g>,
    clusters: BufferHandle<'g>,
    requests: BufferHandle<'g>,
    used: BufferHandle<'g>,
    occluded_instances: BufferHandle<'g>,
    occluded_clusters: BufferHandle<'g>,
    /// A shadow view's draws: cluster index, page.
    shadow_draws: Option<BufferHandle<'g>>,
}

/// What every cull pass reads: the GPU Scene, the pool, the view, the pass's HZB and flags.
#[derive(Clone, Copy)]
struct PassInputs<'g> {
    gpu: GpuScene<'g>,
    pages: BufferHandle<'g>,
    view: View,
    hzb: Option<TextureHandle<'g>>,
    pass: u32,
    /// A shadow view's page table and its list of pages drawn this frame.
    shadow_pages: Option<BufferHandle<'g>>,
    shadow_drawn: Option<BufferHandle<'g>>,
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
            instance_retest_args: compute(shaders.instance_retest_args)?,
            instance_retest: compute(shaders.instance_retest)?,
            node_args: compute(shaders.node_args)?,
            node_cull: compute(shaders.node_cull)?,
            part_args: compute(shaders.part_args)?,
            cluster_cull: compute(shaders.cluster_cull)?,
            cluster_retest_args: compute(shaders.cluster_retest_args)?,
            cluster_retest: compute(shaders.cluster_retest)?,
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
            shadow_raster: device.create_mesh_pipeline(&MeshPipelineDesc {
                amplification: None,
                mesh: shaders.shadow_mesh,
                pixel: Some(shaders.shadow_pixel),
                raster: RasterState {
                    cull: CullMode::None,
                    ..RasterState::default()
                },
                depth: None,
                color_targets: &[],
                multisample: MultisampleState::NONE,
                cache: None,
            })?,
            clear: compute(shaders.clear)?,
            depth: compute(shaders.depth)?,
            hzb_first: compute(shaders.hzb_first)?,
            hzb_down: compute(shaders.hzb_down)?,
            sw_raster: compute(shaders.sw_raster)?,
            lod_error_pixels: 1.0,
            occlusion: true,
            max_nodes: 1 << 21,
            max_parts: 1 << 21,
            max_clusters: 1 << 22,
            max_occluded: 1 << 20,
            max_requests: 4096,
            max_shadow_draws: 1 << 20,
            sw_edge_pixels: 32.0,
        })
    }

    /// Declares the visibility passes of `scene` seen from `view`, reading the GPU Scene `gpu`
    /// (after its scatter) and the page pool `pool` (after its uploads). `meshes` holds the
    /// meshes: the buffers are sized for every drawable instance's whole DAG, within the
    /// budgets.
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        meshes: &Meshes,
        scene: &RenderScene,
        gpu: GpuScene<'g>,
        pool: PagePool<'g>,
        desc: &ViewDesc,
    ) -> VisibilityOutput<'g> {
        let (width, height) = (desc.width, desc.height);
        let vp = view_projection(desc.camera, width, height);
        let previous = desc
            .previous
            .map_or(vp, |p| view_projection(p, width, height));
        let (mut nodes, mut parts, mut clusters, mut levels) = (0u64, 0u64, 0u64, 0u32);
        let mut instances = 0u64;
        for (_, mesh, _) in scene.drawable() {
            let c = meshes.dag_counts(mesh);
            nodes += u64::from(c.nodes);
            parts += u64::from(c.parts);
            clusters += u64::from(c.clusters);
            levels = levels.max(c.depth);
            instances += 1;
        }
        // At least one level, so the part list is written even when nothing is drawn.
        let levels = levels.max(1);
        let node_capacity = capacity(nodes, self.max_nodes);
        let part_capacity = capacity(parts, self.max_parts);
        let cluster_capacity = capacity(clusters, self.max_clusters.min(MAX_CLUSTERS / 2));
        let occluded_capacity = capacity(clusters.max(instances), self.max_occluded);
        let request_capacity = self.max_requests.max(1);
        let rows = scene.instance_rows();
        let ([hzb_w, hzb_h], hzb_mips) = hzb_size(width, height);
        let p = desc.camera.0.translation;
        let view = View {
            view_projection: vp.to_cols_array(),
            previous_view_projection: previous.to_cols_array(),
            planes: planes(&vp),
            eye: [
                p.x,
                p.y,
                p.z,
                height as f32 / (2.0 * marlin_math::scalar::tan(desc.camera.1.fov_y * 0.5)),
            ],
            lod_error_pixels: self.lod_error_pixels,
            instance_rows: rows,
            node_capacity,
            part_capacity,
            cluster_capacity,
            request_capacity,
            occluded_capacity,
            hzb_mips,
            hzb_size: [hzb_w, hzb_h],
            sw_edge_pixels: self.sw_edge_pixels,
            width,
            height,
            mode: 0,
            shadow_pages: 0,
            shadow_pool: 0,
            shadow_page_base: 0,
            shadow_pages_per_side: 0,
            shadow_page_size: 0,
            shadow_pool_pages_per_row: 0,
            shadow_page_offset: [0; 2],
            shadow_drawn_pages: 0,
            shadow_drawn_base: 0,
            shadow_draw_capacity: 0,
        };

        let structured = |n: u32| BufferDesc::structured::<[u32; 2]>(u64::from(n));
        let mut lists = Lists {
            nodes: g.create_buffer("VisNodes", structured(node_capacity)),
            parts: g.create_buffer("VisParts", structured(part_capacity)),
            counters: g.create_buffer(
                "VisCounters",
                BufferDesc::structured::<u32>(counters::WORDS as u64),
            ),
            args: g.create_buffer("VisArgs", BufferDesc::structured::<u32>(24)),
            // Hardware clusters in the first half, software ones in the second.
            clusters: g.create_buffer("VisibleClusters", structured(2 * cluster_capacity)),
            requests: g.create_buffer("VisPageRequests", structured(request_capacity)),
            used: pool.used,
            occluded_instances: g.create_buffer(
                "VisOccludedInstances",
                BufferDesc::structured::<u32>(u64::from(occluded_capacity)),
            ),
            occluded_clusters: g
                .create_buffer("VisOccludedClusters", structured(occluded_capacity)),
            shadow_draws: None,
        };
        let vis = g.create_texture(
            "VisBuffer",
            TextureDesc::new_2d(width, height, Format::R64Uint),
        );
        let depth = g.create_texture(
            "VisDepth",
            TextureDesc::new_2d(width, height, Format::R32Float),
        );
        let hzb = self.occlusion.then(|| {
            g.history(
                HZB,
                desc.id,
                TextureDesc::new_2d(hzb_w, hzb_h, Format::R32Float).with_mips(hzb_mips),
            )
        });
        // Pass 1 tests against the last frame's HZB, when there is one for this camera.
        let previous_hzb = hzb.and_then(|h| h.prev).filter(|_| desc.previous.is_some());
        let two_pass = previous_hzb.is_some();
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
        lists.counters = g.add_compute("VisReset", |b| {
            let counters = b.write_buffer(lists.counters, Access::ComputeWrite);
            (counters, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.reset);
                ctx.push(&CullArgs {
                    counters: ctx.uav(counters),
                    ..CullArgs::default()
                });
                ctx.dispatch(1, 1, 1);
            })
        });

        // Pass 1.
        let first = PassInputs {
            gpu,
            pages: pool.pages,
            view,
            hzb: previous_hzb,
            pass: if two_pass { TEST_HZB } else { 0 },
            shadow_pages: None,
            shadow_drawn: None,
        };
        lists = self.instance_cull(g, lists, first, rows);
        lists = self.traverse(g, lists, first, levels);
        lists = self.cluster_cull(g, lists, first);
        let (mut lists, vis) = self.raster(g, lists, first, vis, width, height);
        let (mut vis, mut depth) = self.split_depth(g, vis, depth, width, height);

        let mut final_hzb = None;
        if let Some(h) = hzb {
            let mut cur = h.cur;
            if two_pass {
                // Pass 2: what pass 1 culled, against an HZB of what it drew.
                cur = self.build_hzb(g, depth, cur, (width, height), [hzb_w, hzb_h], hzb_mips);
                let second = PassInputs {
                    hzb: Some(cur),
                    pass: TEST_HZB | SECOND_PASS,
                    ..first
                };
                lists = self.instance_retest(g, lists, second);
                lists = self.traverse(g, lists, second, levels);
                lists = self.cluster_cull(g, lists, second);
                lists = self.cluster_retest(g, lists, second);
                let (l, v) = self.raster(g, lists, second, vis, width, height);
                lists = l;
                (vis, depth) = self.split_depth(g, v, depth, width, height);
            }
            final_hzb =
                Some(self.build_hzb(g, depth, cur, (width, height), [hzb_w, hzb_h], hzb_mips));
        }
        VisibilityOutput {
            visbuffer: vis,
            depth,
            clusters: lists.clusters,
            counters: lists.counters,
            capacity: cluster_capacity,
            levels,
            pages: pool.pages,
            requests: lists.requests,
            used: lists.used,
            request_capacity,
            hzb: final_hzb,
            two_pass,
        }
    }

    /// Declares shadow views (`Vsm`'s levels, 0010 §5.3): for each of `views`, the passes cull
    /// the drawable instances of `scene` against its orthographic box, pick their LOD cut at its
    /// texel size and draw the clusters over the pages it draws this frame (`page_table` entries
    /// with the render bit, listed in `drawn`, `crate::shadows`) into the physical pool
    /// `shadow_pool` (depth nearest the light, by atomic max), on the hardware, a cluster over
    /// each page on its own. No occlusion test and no page requests; the slots read are marked
    /// in `used`. The views run together: each pass dispatches once per view, so there are as
    /// many passes as for one view. Returns `used` and `shadow_pool` after the passes.
    #[allow(clippy::too_many_arguments, reason = "the shadow views' inputs")]
    pub fn add_shadow_views<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        meshes: &Meshes,
        scene: &RenderScene,
        gpu: GpuScene<'g>,
        pages: BufferHandle<'g>,
        used: BufferHandle<'g>,
        page_table: BufferHandle<'g>,
        drawn: BufferHandle<'g>,
        shadow_pool: TextureHandle<'g>,
        views: &[ShadowView],
    ) -> (BufferHandle<'g>, TextureHandle<'g>) {
        let Some(first) = views.first() else {
            return (used, shadow_pool);
        };
        let (mut nodes, mut parts, mut clusters, mut levels) = (0u64, 0u64, 0u64, 0u32);
        for (_, mesh, _) in scene.drawable() {
            let c = meshes.dag_counts(mesh);
            nodes += u64::from(c.nodes);
            parts += u64::from(c.parts);
            clusters += u64::from(c.clusters);
            levels = levels.max(c.depth);
        }
        let levels = levels.max(1);
        let node_capacity = capacity(nodes, self.max_nodes);
        let part_capacity = capacity(parts, self.max_parts);
        let cluster_capacity = capacity(clusters, self.max_clusters.min(MAX_CLUSTERS / 2));
        let rows = scene.instance_rows();
        // A draw per cluster and page it covers: a few each, and the pages the view draws
        // (at most the pool) under a few clusters each.
        let draw_capacity = capacity(
            4 * (clusters + u64::from(first.pool_pages)),
            self.max_shadow_draws,
        );
        let structured = |n: u32| BufferDesc::structured::<[u32; 2]>(u64::from(n));
        let mut lists: Vec<Lists<'g>> = Vec::with_capacity(views.len());
        let mut inputs: Vec<PassInputs<'g>> = Vec::with_capacity(views.len());
        for desc in views {
            let vp = desc.view_projection;
            let r = desc.resolution;
            let view = View {
                view_projection: vp.to_cols_array(),
                previous_view_projection: vp.to_cols_array(),
                planes: planes(&vp),
                eye: [0.0, 0.0, 0.0, desc.texels_per_unit],
                lod_error_pixels: self.lod_error_pixels,
                instance_rows: rows,
                node_capacity,
                part_capacity,
                cluster_capacity,
                request_capacity: 0,
                occluded_capacity: 1,
                hzb_mips: 0,
                hzb_size: [0; 2],
                sw_edge_pixels: 0.0,
                width: r,
                height: r,
                mode: ORTHOGRAPHIC,
                shadow_pages: 0,
                shadow_pool: 0,
                shadow_page_base: desc.page_base,
                shadow_pages_per_side: desc.pages_per_side,
                shadow_page_size: desc.page_size,
                shadow_pool_pages_per_row: desc.pool_pages_per_row,
                shadow_page_offset: desc.page_offset,
                shadow_drawn_pages: 0,
                shadow_drawn_base: desc.drawn_base,
                shadow_draw_capacity: draw_capacity,
            };
            lists.push(Lists {
                nodes: g.create_buffer("ShadowNodes", structured(node_capacity)),
                parts: g.create_buffer("ShadowParts", structured(part_capacity)),
                counters: g.create_buffer(
                    "ShadowCounters",
                    BufferDesc::structured::<u32>(counters::WORDS as u64),
                ),
                args: g.create_buffer("ShadowArgs", BufferDesc::structured::<u32>(24)),
                // Hardware raster only.
                clusters: g.create_buffer("ShadowClusters", structured(cluster_capacity)),
                requests: g.create_buffer("ShadowRequests", structured(1)),
                used,
                occluded_instances: g
                    .create_buffer("ShadowOccludedInstances", BufferDesc::structured::<u32>(1)),
                occluded_clusters: g.create_buffer("ShadowOccludedClusters", structured(1)),
                shadow_draws: Some(g.create_buffer("ShadowDraws", structured(draw_capacity))),
            });
            inputs.push(PassInputs {
                gpu,
                pages,
                view,
                hzb: None,
                pass: 0,
                shadow_pages: Some(page_table),
                shadow_drawn: Some(drawn),
            });
        }
        let reset: Vec<BufferHandle<'g>> = g.add_compute("ShadowReset", |b| {
            let counters: Vec<_> = lists
                .iter()
                .map(|l| b.write_buffer(l.counters, Access::ComputeWrite))
                .collect();
            (counters.clone(), move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.reset);
                for &c in &counters {
                    ctx.push(&CullArgs {
                        counters: ctx.uav(c),
                        ..CullArgs::default()
                    });
                    ctx.dispatch(1, 1, 1);
                }
            })
        });
        for (l, c) in lists.iter_mut().zip(reset) {
            l.counters = c;
        }
        let instance_groups = rows.div_ceil(64);
        let l = &mut lists;
        let p = &inputs;
        self.cull_passes(
            g,
            "ShadowInstanceCull",
            &self.instance_cull,
            l,
            p,
            None,
            instance_groups,
        );
        for _ in 0..levels {
            self.cull_passes(g, "ShadowNodeArgs", &self.node_args, l, p, None, 1);
            self.cull_passes(g, "ShadowNodeCull", &self.node_cull, l, p, Some(0), 0);
        }
        self.cull_passes(g, "ShadowPartArgs", &self.part_args, l, p, None, 1);
        self.cull_passes(g, "ShadowClusterCull", &self.cluster_cull, l, p, Some(4), 0);
        self.cull_passes(g, "ShadowMeshArgs", &self.finish_args, l, p, None, 1);
        // Each draw is one page, which fills the raster area.
        let page = first.page_size;
        let shadow_pool = g.add_raster("ShadowRaster", |b| {
            b.area(page, page);
            let instances = b.read_buffer(gpu.instance_data, Access::MeshRead);
            let transforms = b.read_buffer(gpu.instance_transforms, Access::MeshRead);
            let meshes = b.read_buffer(gpu.meshes, Access::MeshRead);
            // The clusters are read from the pool through the mesh table's view.
            b.read_buffer(pages, Access::MeshRead);
            let table = b.read_buffer(page_table, Access::MeshRead);
            let pool = b.write_texture(shadow_pool, Access::PixelWrite);
            let draws: Vec<_> = lists
                .iter()
                .zip(&inputs)
                .map(|(l, p)| {
                    let dispatch = b.read_buffer(l.args.range(32, 16), Access::Indirect);
                    let clusters = b.read_buffer(l.clusters, Access::MeshRead);
                    let counters = b.read_buffer(l.counters, Access::MeshRead);
                    let draws = l.shadow_draws.map(|d| b.read_buffer(d, Access::MeshRead));
                    (p.view, dispatch, clusters, counters, draws)
                })
                .collect();
            (pool, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.shadow_raster);
                for &(view, dispatch, clusters, counters, draws) in &draws {
                    let mut v = view;
                    v.shadow_pages = ctx.srv(table);
                    v.shadow_pool = ctx.uav(pool);
                    let view = ctx.upload(&v);
                    ctx.push(&RasterArgs {
                        view,
                        instances: ctx.srv(instances),
                        transforms: ctx.srv(transforms),
                        meshes: ctx.srv(meshes),
                        clusters: ctx.srv(clusters),
                        counters: ctx.srv(counters),
                        vis: 0,
                        shadow_draws: draws.map_or(0, |d| ctx.srv(d)),
                    });
                    ctx.dispatch_mesh_indirect(dispatch, 1, None);
                }
            })
        });
        (lists[0].used, shadow_pool)
    }

    /// The `CullArgs` of a pass, from the views the pass context made of the declared handles.
    /// An indirect pass reads its record from the arguments and does not see them.
    fn args(
        ctx: &PassContext<'_>,
        view: u32,
        l: &Lists<'_>,
        p: &PassInputs<'_>,
        indirect: bool,
    ) -> CullArgs {
        CullArgs {
            view,
            instances: ctx.srv(p.gpu.instance_data),
            transforms: ctx.srv(p.gpu.instance_transforms),
            bounds: ctx.srv(p.gpu.instance_bounds),
            meshes: ctx.srv(p.gpu.meshes),
            nodes: ctx.uav(l.nodes),
            parts: ctx.uav(l.parts),
            counters: ctx.uav(l.counters),
            args: if indirect { 0 } else { ctx.uav(l.args) },
            clusters: ctx.uav(l.clusters),
            requests: ctx.uav(l.requests),
            used: ctx.uav(l.used),
            hzb: p.hzb.map_or(0, |h| ctx.srv(h)),
            occluded_instances: ctx.uav(l.occluded_instances),
            occluded_clusters: ctx.uav(l.occluded_clusters),
            shadow_draws: l.shadow_draws.map_or(0, |d| ctx.uav(d)),
            pass: p.pass,
        }
    }

    /// A cull pass reading everything and writing every list (the passes touch a few of each;
    /// declaring them all keeps the passes in order, as they are anyway). `dispatch` is
    /// `Some(word)` for an indirect dispatch from that word of the arguments.
    #[allow(clippy::too_many_arguments, reason = "one pass's shape")]
    fn cull_pass<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        name: &str,
        pipeline: &'g ComputePipeline,
        lists: Lists<'g>,
        p: PassInputs<'g>,
        indirect: Option<u64>,
        groups: u32,
    ) -> Lists<'g> {
        let mut lists = [lists];
        self.cull_passes(g, name, pipeline, &mut lists, &[p], indirect, groups);
        lists[0]
    }

    /// [`cull_pass`](Self::cull_pass) for several views at once, each with its own lists and
    /// inputs: one pass dispatching once per view, so the views' dispatches run together. The
    /// views share what they read (the GPU Scene, the pool, the HZB and a shadow view's tables,
    /// taken from the first) and the used-slot marks, which they only set.
    #[allow(clippy::too_many_arguments, reason = "one pass's shape")]
    fn cull_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        name: &str,
        pipeline: &'g ComputePipeline,
        lists: &mut [Lists<'g>],
        inputs: &[PassInputs<'g>],
        indirect: Option<u64>,
        groups: u32,
    ) {
        debug_assert_eq!(lists.len(), inputs.len());
        let out = g.add_compute(name, |b| {
            let p = inputs[0];
            let gpu = GpuScene {
                instance_data: b.read_buffer(p.gpu.instance_data, Access::ComputeRead),
                instance_transforms: b.read_buffer(p.gpu.instance_transforms, Access::ComputeRead),
                instance_bounds: b.read_buffer(p.gpu.instance_bounds, Access::ComputeRead),
                meshes: b.read_buffer(p.gpu.meshes, Access::ComputeRead),
                ..p.gpu
            };
            // The clusters are read from the pool through the mesh table's view.
            b.read_buffer(p.pages, Access::ComputeRead);
            let hzb = p.hzb.map(|h| b.read_texture(h, Access::ComputeRead));
            let shadow_pages = p
                .shadow_pages
                .map(|t| b.read_buffer(t, Access::ComputeRead));
            let shadow_drawn = p
                .shadow_drawn
                .map(|t| b.read_buffer(t, Access::ComputeRead));
            let used = b.write_buffer(lists[0].used, Access::ComputeWrite);
            let views: Vec<_> = lists
                .iter()
                .zip(inputs)
                .map(|(lists, p)| {
                    let dispatch = indirect
                        .map(|at| b.read_buffer(lists.args.range(at * 4, 16), Access::Indirect));
                    let l = Lists {
                        nodes: b.write_buffer(lists.nodes, Access::ComputeWrite),
                        parts: b.write_buffer(lists.parts, Access::ComputeWrite),
                        counters: b.write_buffer(lists.counters, Access::ComputeWrite),
                        // An indirect pass reads its record: the args passes write the args.
                        args: if indirect.is_some() {
                            lists.args
                        } else {
                            b.write_buffer(lists.args, Access::ComputeWrite)
                        },
                        clusters: b.write_buffer(lists.clusters, Access::ComputeWrite),
                        requests: b.write_buffer(lists.requests, Access::ComputeWrite),
                        used,
                        occluded_instances: b
                            .write_buffer(lists.occluded_instances, Access::ComputeWrite),
                        occluded_clusters: b
                            .write_buffer(lists.occluded_clusters, Access::ComputeWrite),
                        shadow_draws: lists
                            .shadow_draws
                            .map(|d| b.write_buffer(d, Access::ComputeWrite)),
                    };
                    let inputs = PassInputs {
                        gpu,
                        hzb,
                        shadow_pages,
                        shadow_drawn,
                        ..*p
                    };
                    (l, inputs, dispatch)
                })
                .collect();
            let out: Vec<Lists<'g>> = views.iter().map(|v| v.0).collect();
            (out, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(pipeline);
                for (l, inputs, dispatch) in &views {
                    let mut view = inputs.view;
                    if let Some(t) = inputs.shadow_pages {
                        view.shadow_pages = ctx.srv(t);
                    }
                    if let Some(t) = inputs.shadow_drawn {
                        view.shadow_drawn_pages = ctx.srv(t);
                    }
                    let view = ctx.upload(&view);
                    let args = Self::args(ctx, view, l, inputs, dispatch.is_some());
                    ctx.push(&args);
                    match *dispatch {
                        Some(d) => ctx.dispatch_indirect(d, 1, None),
                        None => ctx.dispatch(groups, 1, 1),
                    }
                }
            })
        });
        lists.copy_from_slice(&out);
    }

    fn instance_cull<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        lists: Lists<'g>,
        p: PassInputs<'g>,
        rows: u32,
    ) -> Lists<'g> {
        self.cull_pass(
            g,
            "InstanceCull",
            &self.instance_cull,
            lists,
            p,
            None,
            rows.div_ceil(64),
        )
    }

    fn instance_retest<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        lists: Lists<'g>,
        p: PassInputs<'g>,
    ) -> Lists<'g> {
        let lists = self.cull_pass(
            g,
            "VisInstanceRetestArgs",
            &self.instance_retest_args,
            lists,
            p,
            None,
            1,
        );
        self.cull_pass(
            g,
            "InstanceRetest",
            &self.instance_retest,
            lists,
            p,
            Some(12),
            0,
        )
    }

    fn traverse<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        mut lists: Lists<'g>,
        p: PassInputs<'g>,
        levels: u32,
    ) -> Lists<'g> {
        for _ in 0..levels {
            lists = self.cull_pass(g, "VisNodeArgs", &self.node_args, lists, p, None, 1);
            lists = self.cull_pass(g, "NodeCull", &self.node_cull, lists, p, Some(0), 0);
        }
        lists
    }

    fn cluster_cull<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        lists: Lists<'g>,
        p: PassInputs<'g>,
    ) -> Lists<'g> {
        let lists = self.cull_pass(g, "VisPartArgs", &self.part_args, lists, p, None, 1);
        self.cull_pass(g, "ClusterCull", &self.cluster_cull, lists, p, Some(4), 0)
    }

    fn cluster_retest<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        lists: Lists<'g>,
        p: PassInputs<'g>,
    ) -> Lists<'g> {
        let lists = self.cull_pass(
            g,
            "VisClusterRetestArgs",
            &self.cluster_retest_args,
            lists,
            p,
            None,
            1,
        );
        self.cull_pass(
            g,
            "ClusterRetest",
            &self.cluster_retest,
            lists,
            p,
            Some(16),
            0,
        )
    }

    /// The pass's mesh dispatch and its draw into `vis`.
    fn raster<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        lists: Lists<'g>,
        p: PassInputs<'g>,
        vis: TextureHandle<'g>,
        width: u32,
        height: u32,
    ) -> (Lists<'g>, TextureHandle<'g>) {
        let lists = self.cull_pass(g, "VisMeshArgs", &self.finish_args, lists, p, None, 1);
        let view = p.view;
        let vis = g.add_raster("VisRaster", |b| {
            b.area(width, height);
            let dispatch = b.read_buffer(lists.args.range(32, 16), Access::Indirect);
            let instances = b.read_buffer(p.gpu.instance_data, Access::MeshRead);
            let transforms = b.read_buffer(p.gpu.instance_transforms, Access::MeshRead);
            let meshes = b.read_buffer(p.gpu.meshes, Access::MeshRead);
            let clusters = b.read_buffer(lists.clusters, Access::MeshRead);
            let counters = b.read_buffer(lists.counters, Access::MeshRead);
            // The clusters are read from the pool through the mesh table's view.
            b.read_buffer(p.pages, Access::MeshRead);
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
                    shadow_draws: 0,
                });
                ctx.dispatch_mesh_indirect(dispatch, 1, None);
            })
        });
        let vis = g.add_compute("SwRaster", |b| {
            let dispatch = b.read_buffer(lists.args.range(80, 16), Access::Indirect);
            let instances = b.read_buffer(p.gpu.instance_data, Access::ComputeRead);
            let transforms = b.read_buffer(p.gpu.instance_transforms, Access::ComputeRead);
            let meshes = b.read_buffer(p.gpu.meshes, Access::ComputeRead);
            let clusters = b.read_buffer(lists.clusters, Access::ComputeRead);
            let counters = b.read_buffer(lists.counters, Access::ComputeRead);
            // The clusters are read from the pool through the mesh table's view.
            b.read_buffer(p.pages, Access::ComputeRead);
            let vis = b.write_texture(vis, Access::ComputeWrite);
            (vis, move |ctx: &mut PassContext<'_>| {
                let view = ctx.upload(&view);
                ctx.set_pipeline(&self.sw_raster);
                ctx.push(&RasterArgs {
                    view,
                    instances: ctx.srv(instances),
                    transforms: ctx.srv(transforms),
                    meshes: ctx.srv(meshes),
                    clusters: ctx.srv(clusters),
                    counters: ctx.srv(counters),
                    vis: ctx.uav(vis),
                    shadow_draws: 0,
                });
                ctx.dispatch_indirect(dispatch, 1, None);
            })
        });
        (lists, vis)
    }

    /// The depth half of `vis` into `depth`.
    fn split_depth<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        vis: TextureHandle<'g>,
        depth: TextureHandle<'g>,
        width: u32,
        height: u32,
    ) -> (TextureHandle<'g>, TextureHandle<'g>) {
        g.add_compute("VisDepth", |b| {
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
                ctx.dispatch(width.div_ceil(8), height.div_ceil(8), 1);
            })
        })
    }

    /// The HZB of `depth` (a `screen`-sized view) into `hzb` (mip 0 `size`, `mips` mips).
    fn build_hzb<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        depth: TextureHandle<'g>,
        mut hzb: TextureHandle<'g>,
        screen: (u32, u32),
        size: [u32; 2],
        mips: u32,
    ) -> TextureHandle<'g> {
        let mut source = (screen.0, screen.1);
        for mip in 0..mips {
            let (w, h) = ((size[0] >> mip).max(1), (size[1] >> mip).max(1));
            let (pipeline, name) = if mip == 0 {
                (&self.hzb_first, "HzbFirst")
            } else {
                (&self.hzb_down, "HzbDown")
            };
            let from = source;
            hzb = g.add_compute(name, |b| {
                let input = if mip == 0 {
                    b.read_texture(depth, Access::ComputeRead)
                } else {
                    b.read_texture(hzb.mip(mip - 1), Access::ComputeRead)
                };
                let out = b.write_texture(hzb.mip(mip), Access::ComputeWrite);
                (out, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(pipeline);
                    ctx.push(&HzbArgs {
                        source: ctx.srv(input),
                        output: ctx.uav(out.mip(mip)),
                        source_width: from.0,
                        source_height: from.1,
                        width: w,
                        height: h,
                    });
                    ctx.dispatch(w.div_ceil(8), h.div_ceil(8), 1);
                })
            });
            source = (w, h);
        }
        hzb
    }
}

/// One shadow view (a level of `Vsm`'s clipmap) for [`Visibility::add_shadow_views`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ShadowView {
    /// World to the view's clip space: orthographic, reverse-Z (nearer the light is larger).
    pub view_projection: Mat4,
    /// Texels per world unit: the LOD cut's scale.
    pub texels_per_unit: f32,
    /// Texels per side of the view.
    pub resolution: u32,
    /// Texels per side of a page, pages per side of the view, and the view's first entry in
    /// the page table.
    pub page_size: u32,
    pub pages_per_side: u32,
    pub page_base: u32,
    /// Physical pages per row of the pool, and pages in it.
    pub pool_pages_per_row: u32,
    pub pool_pages: u32,
    /// The view's list of pages drawn this frame in `drawn` (`Vsm`'s): its count, then each
    /// page (`x | y << 16`, from the view's first).
    pub drawn_base: u32,
    /// Where the view's first page sits in its table (pages wrap around, so a page keeps its
    /// slot while the view moves): page p is in slot `(p + page_offset) mod pages_per_side`.
    pub page_offset: [u32; 2],
}
