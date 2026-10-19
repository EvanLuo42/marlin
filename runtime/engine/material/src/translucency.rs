//! The forward pass of translucent surfaces (0009 §8, R4e): instances whose material type is
//! translucent ([`Materials::register_translucent`]) are culled by marlin-render's translucent
//! view ([`marlin_render::Visibility::add_translucent_view`]) and drawn here over the lit scene,
//! blended. `Material/Translucent.mshader`'s file comment describes the passes.
//!
//! **Order**: the instances are sorted on the CPU, back to front by their world spheres'
//! centers, and each is one indirect mesh draw with its type's pipeline, in that order (the
//! user's choice, 2026-10-19: one draw cannot change pipelines, so 0009 §8's GPU sort waits
//! for a generated all-type switch). The triangles inside an instance are not sorted.

use alloc::vec;
use alloc::vec::Vec;

use marlin_math::Transform;
use marlin_render::raytracing::RtScene;
use marlin_render::sharc::{SharcOutput, SharcSources};
use marlin_render::{Camera, Environment, GpuScene, RenderScene, TranslucentClusters};
use marlin_render_graph::{Access, BufferDesc, PassContext, RenderGraph, TextureHandle};
use marlin_rhi::{LoadOp, StoreOp};
use marlin_serial::cooked::Pod;

use crate::Materials;

/// `Marlin.Engine.Material.BinArgs`.
#[repr(C)]
#[derive(Pod, Clone, Copy, Default)]
pub(crate) struct BinArgs {
    clusters: u32,
    counters: u32,
    capacity: u32,
    slot_of_row: u32,
    slots: u32,
    counts: u32,
    offsets: u32,
    args: u32,
    sorted: u32,
}

/// `Marlin.Engine.Material.ForwardView`.
#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct ForwardView {
    view_projection: [f32; 16],
    eye: [f32; 4],
    sun_direction: [f32; 4],
    sun: [f32; 4],
    sky: [f32; 4],
    cache: SharcSources,
    has_cache: u32,
    spare: u32,
}

/// `Marlin.Engine.Material.ForwardArgs<M>` (and `TranslucentMeshArgs`).
#[repr(C)]
#[derive(Pod, Clone, Copy, Default)]
pub(crate) struct ForwardArgs {
    view: u32,
    instances: u32,
    transforms: u32,
    meshes: u32,
    materials: u32,
    params: u32,
    sorted: u32,
    depth: u32,
    tlas: u32,
    offsets: u32,
    slot: u32,
}

/// What [`Materials::add_translucent_passes`] reads of the frame.
#[derive(Clone, Copy)]
pub struct TranslucentInputs<'g> {
    /// The translucent view's culling.
    pub clusters: TranslucentClusters<'g>,
    /// The opaque depth (`R32Float`, reverse-Z), which the pass tests against.
    pub depth: TextureHandle<'g>,
    /// What the sun's shadow rays are traced against.
    pub rt: RtScene<'g>,
    /// The world cache, the light around a translucent surface; without it, the sky's.
    pub cache: Option<SharcOutput<'g>>,
    pub environment: Environment,
}

impl Materials {
    /// Declares the translucent surfaces of `scene` drawn over `color` (the lit HDR scene) as
    /// `camera` sees them (`width` × `height`); returns `color` after. Nothing is declared
    /// without translucent instances.
    #[allow(clippy::too_many_arguments, reason = "one view's inputs")]
    pub fn add_translucent_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        scene: &RenderScene,
        gpu: GpuScene<'g>,
        color: TextureHandle<'g>,
        inputs: TranslucentInputs<'g>,
        camera: (Transform, Camera),
        width: u32,
        height: u32,
    ) -> TextureHandle<'g> {
        // Back to front by the spheres' centers, each with its permutation.
        let eye = camera.0.translation;
        let mut order: Vec<(u32, u32, f32)> = scene
            .translucent_instances()
            .into_iter()
            .filter_map(|(row, [x, y, z, _])| {
                let permutation = scene
                    .instance_material_row(row)
                    .and_then(|m| scene.material_permutation(m))?;
                let forward = self.types().get(permutation as usize)?.forward.as_ref();
                forward?;
                let d = marlin_math::Vec3::new(x, y, z) - eye;
                Some((row, permutation, d.length_squared()))
            })
            .collect();
        if order.is_empty() {
            return color;
        }
        order.sort_by(|a, b| b.2.total_cmp(&a.2));
        let slots = order.len() as u32;
        let mut slot_of_row = vec![u32::MAX; scene.instance_rows().max(1) as usize];
        for (slot, &(row, _, _)) in order.iter().enumerate() {
            slot_of_row[row as usize] = slot as u32;
        }
        let capacity = inputs.clusters.capacity;
        let words = |n: u32| BufferDesc::structured::<u32>(u64::from(n.max(1)));
        let counts = g.create_buffer("TlCounts", words(slots));
        let offsets = g.create_buffer("TlOffsets", words(slots));
        let args = g.create_buffer("TlArgs", words(4 * slots));
        let sorted = g.create_buffer(
            "TlSorted",
            BufferDesc::structured::<[u32; 2]>(u64::from(capacity.max(1))),
        );
        let counts = g.add_compute("TlClear", |b| {
            let counts = b.write_buffer(counts, Access::Clear);
            (counts, move |ctx: &mut PassContext<'_>| {
                ctx.fill_buffer(counts, 0)
            })
        });
        let clusters = inputs.clusters;
        let groups = capacity.div_ceil(64).max(1);
        let slot_table = &*g.arena().alloc_slice_copy(&slot_of_row);
        // Count, offsets, scatter: one pass each, reading the same lists.
        let bin = |g: &mut RenderGraph<'g>,
                   name: &str,
                   pipeline: &'g marlin_rhi::ComputePipeline,
                   groups: u32,
                   counts: marlin_render_graph::BufferHandle<'g>,
                   offsets: marlin_render_graph::BufferHandle<'g>,
                   args: marlin_render_graph::BufferHandle<'g>,
                   sorted: marlin_render_graph::BufferHandle<'g>| {
            g.add_compute(name, |b| {
                let list = b.read_buffer(clusters.clusters, Access::ComputeRead);
                let counters = b.read_buffer(clusters.counters, Access::ComputeRead);
                let counts = b.write_buffer(counts, Access::ComputeWrite);
                let offsets = b.write_buffer(offsets, Access::ComputeWrite);
                let args = b.write_buffer(args, Access::ComputeWrite);
                let sorted = b.write_buffer(sorted, Access::ComputeWrite);
                (
                    (counts, offsets, args, sorted),
                    move |ctx: &mut PassContext<'_>| {
                        ctx.set_pipeline(pipeline);
                        let slot_of_row = ctx.upload_slice(slot_table);
                        ctx.push(&BinArgs {
                            clusters: ctx.srv(list),
                            counters: ctx.srv(counters),
                            capacity,
                            slot_of_row,
                            slots,
                            counts: ctx.uav(counts),
                            offsets: ctx.uav(offsets),
                            args: ctx.uav(args),
                            sorted: ctx.uav(sorted),
                        });
                        ctx.dispatch(groups, 1, 1);
                    },
                )
            })
        };
        let (counts, offsets, args, sorted) = bin(
            g,
            "TranslucentCount",
            &self.translucent_count,
            groups,
            counts,
            offsets,
            args,
            sorted,
        );
        let (counts, offsets, args, sorted) = bin(
            g,
            "TranslucentOffsets",
            &self.translucent_offsets,
            1,
            counts,
            offsets,
            args,
            sorted,
        );
        let (_, offsets, args, sorted) = bin(
            g,
            "TranslucentScatter",
            &self.translucent_scatter,
            groups,
            counts,
            offsets,
            args,
            sorted,
        );

        let env = inputs.environment;
        let d = env.light_direction.normalize_or_zero();
        let sun = env.sun_illuminance / core::f32::consts::PI;
        let has_sun = sun > 0.0 && d != marlin_math::Vec3::ZERO;
        let [sr, sg, sb] = env.light_color;
        let [ar, ag, ab] = env.ambient;
        let view_projection = marlin_render::view_projection(camera, width, height);
        let view = ForwardView {
            view_projection: view_projection.to_cols_array(),
            eye: [eye.x, eye.y, eye.z, 0.0],
            sun_direction: [d.x, d.y, d.z, f32::from(u8::from(has_sun))],
            sun: [sr * sun, sg * sun, sb * sun, 0.0],
            sky: [ar, ag, ab, 0.0],
            cache: SharcSources::default(),
            has_cache: u32::from(inputs.cache.is_some()),
            spare: 0,
        };
        let draws = &*g.arena().alloc_slice_copy(
            &order
                .iter()
                .map(|&(_, permutation, _)| permutation)
                .collect::<Vec<u32>>(),
        );
        let rt = inputs.rt;
        let cache = inputs.cache;
        let depth = inputs.depth;
        g.add_raster("Translucent", |b| {
            let color = b.color(color, LoadOp::Load, StoreOp::Store);
            let instances = b.read_buffer(gpu.instance_data, Access::MeshRead);
            let transforms = b.read_buffer(gpu.instance_transforms, Access::MeshRead);
            let meshes = b.read_buffer(gpu.meshes, Access::MeshRead);
            let materials = b.read_buffer(gpu.materials, Access::PixelRead);
            let params: Vec<_> = gpu
                .material_params
                .iter()
                .map(|&p| b.read_buffer(p, Access::PixelRead))
                .collect();
            // The clusters are read from the pool through the mesh table's view.
            b.read_buffer(clusters.pages, Access::MeshRead);
            let sorted = b.read_buffer(sorted, Access::MeshRead);
            let offsets = b.read_buffer(offsets, Access::MeshRead);
            let depth = b.read_texture(depth, Access::PixelRead);
            rt.read(b);
            let cache = cache.map(|c| c.read(b));
            let records: Vec<_> = (0..slots)
                .map(|slot| b.read_buffer(args.range(u64::from(slot) * 16, 16), Access::Indirect))
                .collect();
            (color, move |ctx: &mut PassContext<'_>| {
                let mut v = view;
                if let Some(c) = &cache {
                    v.cache = c.bind(ctx);
                }
                let view = ctx.upload(&v);
                let shared = ForwardArgs {
                    view,
                    instances: ctx.srv(instances),
                    transforms: ctx.srv(transforms),
                    meshes: ctx.srv(meshes),
                    materials: ctx.srv(materials),
                    params: 0,
                    sorted: ctx.srv(sorted),
                    depth: ctx.srv(depth),
                    tlas: rt.index,
                    offsets: ctx.srv(offsets),
                    slot: 0,
                };
                for (slot, &permutation) in draws.iter().enumerate() {
                    let p = permutation as usize;
                    let Some(pipeline) = &self.types()[p].forward else {
                        continue;
                    };
                    ctx.set_pipeline(pipeline);
                    ctx.push(&ForwardArgs {
                        params: params.get(p).map_or(0, |&h| ctx.srv(h)),
                        slot: slot as u32,
                        ..shared
                    });
                    ctx.dispatch_mesh_indirect(records[slot], 1, None);
                }
            })
        })
    }
}
