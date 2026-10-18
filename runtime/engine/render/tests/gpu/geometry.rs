//! Resident geometry: cooked LOD pages decoded on the GPU by the engine's own decode
//! (`Marlin.Engine.Scene`) match the CPU decode, and quantized positions round-trip; instances
//! draw the LOD their distance allows.

use marlin_gpu_test_support::crate_shader;
use marlin_math::{Transform, Vec3};
use marlin_render::Environment;
use marlin_render::asset::Mesh;
use marlin_render::asset::PositionPrecision;
use marlin_render_graph::{Access, BufferDesc, PassContext};
use marlin_rhi::{ComputePipeline, ComputePipelineDesc};
use marlin_serial::cooked::Pod;

use crate::support::cook::{Cooked, cook_with, sphere, sphere_source};
use crate::support::{Harness, words};

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct DecodeArgs {
    meshes: u32,
    geometry: u32,
    output: u32,
    mesh: u32,
    lod: u32,
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

/// Every vertex of every LOD of `cooked`, decoded on the GPU: (position, normal) per vertex.
fn gpu_decode(h: &mut Harness, cooked: &Cooked) -> Vec<Vec<([f32; 3], [f32; 3])>> {
    let pipeline = decode_pipeline(h);
    let (mesh, handle) = h.upload(cooked);
    let geometry = h.renderer.resident_mesh(handle).unwrap().geometry;
    let view = cooked.view();
    let counts: Vec<u32> = view.lods.iter().map(|l| l.vertex_count).collect();
    let (_, _, tickets) = h.frame_with(|g, gpu| {
        let mut tickets = Vec::new();
        for (lod, &count) in counts.iter().enumerate() {
            let out = g.create_buffer("Decoded", BufferDesc::new(u64::from(count) * 24));
            let out = g.add_compute("DecodeVertices", |b| {
                let meshes = b.read_buffer(gpu.meshes, Access::ComputeRead);
                let out = b.write_buffer(out, Access::ComputeWrite);
                (out, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(pipeline);
                    ctx.push(&DecodeArgs {
                        meshes: ctx.srv(meshes),
                        geometry,
                        output: ctx.uav(out),
                        mesh: mesh.index(),
                        lod: lod as u32,
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
fn the_gpu_decodes_every_lod_as_the_cpu_does() {
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
        assert!(view.lods.len() > 1, "a LOD chain");
        let gpu = gpu_decode(&mut h, &cooked);
        let scale = view.header.position_scale;
        for (lod, decoded) in gpu.iter().enumerate() {
            let l = &view.lods[lod];
            let (cpu, _) = view
                .decode_lod(lod, &cooked.pages[l.bulk as usize])
                .unwrap();
            assert_eq!(decoded.len(), cpu.len());
            for (g, c) in decoded.iter().zip(&cpu) {
                for i in 0..3 {
                    // A fused multiply-add on the GPU may round once less.
                    assert!(
                        (g.0[i] - c.position[i]).abs() <= 1e-6,
                        "{precision:?} LOD {lod}: {g:?} {c:?}"
                    );
                }
                assert!(dot(g.1, c.normal) > 0.999_99, "{g:?} {c:?}");
            }
        }
        // The round trip: LOD 0's positions are on the sphere to within half a quantization
        // step per axis (exactly, with float positions).
        let step = match precision {
            PositionPrecision::Quantized => scale.map(|s| s * 0.5),
            PositionPrecision::Float => [0.0; 3],
        };
        let slack = (dot(step, step)).sqrt() + 1e-6;
        for (p, n) in &gpu[0] {
            let r = dot(*p, *p).sqrt();
            assert!((r - 0.5).abs() <= slack, "{precision:?}: {p:?} at {r}");
            // Smooth normals point out of the sphere.
            assert!(dot(*n, p.map(|c| c / r)) > 0.99, "{n:?} at {p:?}");
        }
    }
}

/// At 64×64 pixels even close up a pixel hides LOD 1's error, so the near frame may already
/// draw a coarser LOD; farther frames never draw finer ones.
#[test]
fn far_instances_draw_coarser_lods() {
    let mut h = Harness::new(Environment::default());
    let s = sphere(32, 64);
    let lods: Vec<u32> = s.view().lods.iter().map(|l| l.triangle_count).collect();
    assert!(lods.len() >= 3, "{lods:?}");
    let (mesh, _) = h.upload(&s);
    h.camera_at(0.0);
    let id = h.add(
        mesh,
        Transform::from_translation(Vec3::new(0.0, 0.0, -1.5)),
        [1.0; 4],
    );
    let (_, near) = h.render();
    let mut last = near.triangles;
    for z in [-20.0, -100.0, -1000.0] {
        h.set(
            id,
            marlin_render::scene::InstanceUpdate::Transform(Transform::from_translation(
                Vec3::new(0.0, 0.0, z),
            )),
        );
        let (_, stats) = h.render();
        assert!(
            stats.triangles <= last,
            "{z}: {} after {last}",
            stats.triangles
        );
        assert!(lods.contains(&(stats.triangles as u32)));
        last = stats.triangles;
    }
    assert_eq!(
        last,
        u64::from(*lods.last().unwrap()),
        "far away: the last LOD"
    );
    // A tiny error budget: LOD 0 close up; none allowed: LOD 0 everywhere.
    h.set(
        id,
        marlin_render::scene::InstanceUpdate::Transform(Transform::from_translation(Vec3::new(
            0.0, 0.0, -1.5,
        ))),
    );
    h.renderer.lod_error_pixels = 1e-6;
    let (_, stats) = h.render();
    assert_eq!(stats.triangles, u64::from(lods[0]));
    h.set(
        id,
        marlin_render::scene::InstanceUpdate::Transform(Transform::from_translation(Vec3::new(
            0.0, 0.0, -1000.0,
        ))),
    );
    h.renderer.lod_error_pixels = 0.0;
    let (_, stats) = h.render();
    assert_eq!(stats.triangles, u64::from(lods[0]));
}
