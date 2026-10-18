//! Resident geometry: a mesh's cluster DAG pages in the page pool decode on the GPU by the
//! engine's own decode (`Marlin.Engine.Scene`) as on the CPU, quantized positions round-trip,
//! and the pool's slots come back when a mesh is released.

use marlin_gpu_test_support::crate_shader;
use marlin_render::Environment;
use marlin_render::asset::Mesh;
use marlin_render::asset::PositionPrecision;
use marlin_render::asset::cluster::read_cluster_attributes;
use marlin_render_graph::{Access, BufferDesc, PassContext};
use marlin_rhi::{ComputePipeline, ComputePipelineDesc};
use marlin_serial::cooked::Pod;

use crate::support::cook::{Cooked, cook_with, sphere, sphere_source};
use crate::support::{Harness, POOL_PAGES, words};

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct DecodeArgs {
    meshes: u32,
    output: u32,
    mesh: u32,
    slot: u32,
}

/// The test's pipeline, for the binary's life (graph passes borrow it for any frame).
fn decode_pipeline(h: &Harness) -> &'static ComputePipeline {
    Box::leak(Box::new(
        h.device
            .create_compute_pipeline(&ComputePipelineDesc {
                shader: crate_shader!("Render.DecodeVertices"),
                cache: None,
            })
            .unwrap(),
    ))
}

type Decoded = Vec<([f32; 3], [f32; 3])>;

/// Every vertex of every DAG page of `cooked`, decoded on the GPU from the page pool:
/// (position, normal) per vertex.
fn gpu_decode(h: &mut Harness, cooked: &Cooked) -> Vec<Decoded> {
    let pipeline = decode_pipeline(h);
    let (mesh, handle) = h.upload(cooked);
    let view = cooked.view();
    let dag = view.cluster_dag;
    let pages: Vec<(u32, u32)> = (0..dag.pages.len())
        .map(|p| {
            let bytes = &cooked.pages[dag.pages[p].bulk as usize];
            let count = dag.page(p, bytes).unwrap().header.vertex_count;
            (h.meshes.page_slot(handle, p).unwrap(), count)
        })
        .collect();
    let tickets = h.frame_with(|g, gpu| {
        let mut tickets = Vec::new();
        for &(slot, count) in &pages {
            let out = g.create_buffer("Decoded", BufferDesc::new(u64::from(count) * 24));
            let out = g.add_compute("DecodeVertices", |b| {
                let meshes = b.read_buffer(gpu.meshes, Access::ComputeRead);
                let out = b.write_buffer(out, Access::ComputeWrite);
                (out, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(pipeline);
                    ctx.push(&DecodeArgs {
                        meshes: ctx.srv(meshes),
                        output: ctx.uav(out),
                        mesh: mesh.index(),
                        slot,
                    });
                    ctx.dispatch(count.div_ceil(64), 1, 1);
                })
            });
            tickets.push(g.readback(out));
        }
        tickets
    });
    tickets
        .into_iter()
        .map(|t| {
            let w = words(&h.wait_readback(t));
            w.as_chunks::<6>()
                .0
                .iter()
                .map(|v| {
                    let f = v.map(f32::from_bits);
                    ([f[0], f[1], f[2]], [f[3], f[4], f[5]])
                })
                .collect()
        })
        .collect()
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[test]
fn the_gpu_decodes_every_page_as_the_cpu_does() {
    let mut h = Harness::new(Environment::default());
    for precision in [PositionPrecision::Quantized, PositionPrecision::Float] {
        let (positions, normals, indices) = sphere_source(24, 48);
        let cooked = cook_with(
            &positions,
            &normals,
            &indices,
            Mesh {
                precision,
                ..Mesh::default()
            },
        );
        let view = cooked.view();
        let dag = view.cluster_dag;
        assert!(dag.header.level_count > 1, "a DAG");
        let gpu = gpu_decode(&mut h, &cooked);
        let (offset, scale) = (view.header.position_offset, view.header.position_scale);
        let stride = view.header.attribute_stride as usize;
        // The round trip: every position is on the sphere to within half a quantization step
        // per axis (exactly, with float positions): simplification only moves vertices onto
        // other source vertices.
        let step = match precision {
            PositionPrecision::Quantized => scale.map(|s| s * 0.5),
            PositionPrecision::Float => [0.0; 3],
        };
        let slack = (dot(step, step)).sqrt() + 1e-6;
        for (page, decoded) in gpu.iter().enumerate() {
            let pv = dag
                .page(page, &cooked.pages[dag.pages[page].bulk as usize])
                .unwrap();
            assert_eq!(decoded.len(), pv.header.vertex_count as usize);
            for (v, (p, n)) in decoded.iter().enumerate() {
                let c = pv.position(v, offset, scale);
                let a = read_cluster_attributes(pv.attributes(), stride, v, &[]);
                for i in 0..3 {
                    // A fused multiply-add on the GPU may round once less.
                    assert!(
                        (p[i] - c[i]).abs() <= 1e-6,
                        "{precision:?} page {page}: {p:?} {c:?}"
                    );
                }
                assert!(dot(*n, a.normal) > 0.999_99, "{n:?} {:?}", a.normal);
                let r = dot(*p, *p).sqrt();
                assert!((r - 0.5).abs() <= slack, "{precision:?}: {p:?} at {r}");
                // Smooth normals point out of the sphere.
                assert!(dot(*n, p.map(|c| c / r)) > 0.99, "{n:?} at {p:?}");
            }
        }
    }
}

#[test]
fn released_meshes_give_their_pool_slots_back() {
    let mut h = Harness::new(Environment::default());
    let free = h.meshes.free_pages();
    assert_eq!(free, POOL_PAGES as usize - 1, "the cube's one page");
    let ball = sphere(64, 128);
    let pages = ball.view().cluster_dag.pages.len();
    assert!(pages > 1, "{pages} pages");
    let (_, handle) = h.upload(&ball);
    assert_eq!(h.meshes.free_pages(), free - pages);
    let slots: Vec<u32> = (0..pages)
        .map(|p| h.meshes.page_slot(handle, p).unwrap())
        .collect();
    let mut sorted = slots.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), pages, "a slot each");
    h.wait_idle();
    // SAFETY: the GPU is idle.
    unsafe { h.meshes.release_mesh(handle) };
    assert_eq!(h.meshes.free_pages(), free);
    assert!(h.meshes.resident_mesh(handle).is_none());
    // The next upload takes the same slots.
    let (_, again) = h.upload(&ball);
    let reused: Vec<u32> = (0..pages)
        .map(|p| h.meshes.page_slot(again, p).unwrap())
        .collect();
    assert_eq!(reused, slots);
}

#[test]
fn a_mesh_that_does_not_fit_in_the_pool_is_refused() {
    let mut h = Harness::new(Environment::default());
    let ball = sphere(64, 128);
    let pages = ball.view().cluster_dag.pages.len();
    let mut uploaded = 0;
    loop {
        match h.meshes.upload_mesh(h.device, &ball.view(), &ball.pages()) {
            Ok(_) => uploaded += 1,
            Err(e) => {
                assert_eq!(e.kind(), marlin_rhi::ErrorKind::OutOfMemory, "{e}");
                break;
            }
        }
    }
    assert_eq!(uploaded, (POOL_PAGES as usize - 1) / pages);
    assert!(h.meshes.free_pages() < pages);
}
