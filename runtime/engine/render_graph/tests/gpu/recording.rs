//! Recording: lists recorded as jobs, passes split with `ctx.parallel`, GPU timings, captures.

use std::sync::OnceLock;

use marlin_gpu_test_support::{solid_constants, solid_pipeline};
use marlin_jobs::{Config, JobSystem};
use marlin_render_graph::{
    Access, BufferDesc, BufferHandle, CAPTURE_SCOPE, FrameContext, PassContext, RenderGraph,
    TextureDesc,
};
use marlin_rhi::{ColorTargetState, ComputePipeline, Format, GraphicsPipeline, LoadOp, StoreOp};
use marlin_serial::cooked::Pod;

use crate::frame::{context, device, finish, words};

const WORDS: u32 = 64;

fn jobs() -> &'static JobSystem {
    static JOBS: OnceLock<JobSystem> = OnceLock::new();
    JOBS.get_or_init(|| {
        JobSystem::new(Config {
            workers: 4,
            // Driver and validation-layer calls need deeper stacks than game jobs.
            fiber_stack_size: 1 << 20,
            ..Config::default()
        })
        .unwrap()
    })
}

fn fill() -> &'static ComputePipeline {
    static P: OnceLock<ComputePipeline> = OnceLock::new();
    P.get_or_init(|| marlin_gpu_test_support::compute_pipeline(device(), "Fill"))
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct FillArgs {
    output: u32,
    base: u32,
}

fn expected(base: u32) -> Vec<u32> {
    (0..WORDS).map(|i| base + 3 * i).collect()
}

/// `count` passes, each filling a buffer of its own from `100 * i`; each claims to take long to
/// record, so every one gets a list of its own.
fn many_passes(g: &mut RenderGraph<'_>, count: u32) -> Vec<marlin_render_graph::Readback> {
    (0..count)
        .map(|i| {
            let out = g.create_buffer("Out", BufferDesc::new(u64::from(WORDS) * 4));
            let out = g.add_compute("Fill", |b| {
                b.cost(500);
                let out = b.write_buffer(out, Access::ComputeWrite);
                (out, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(fill());
                    ctx.push(&FillArgs {
                        output: ctx.uav(out),
                        base: 100 * i,
                    });
                    ctx.dispatch(WORDS / 64, 1, 1);
                })
            });
            g.readback(out)
        })
        .collect()
}

#[test]
fn lists_recorded_as_jobs_give_what_one_thread_gives() {
    const PASSES: u32 = 24;
    let run = |frame: &mut FrameContext, jobs: Option<&JobSystem>| {
        let tickets = frame
            .execute_on(device(), jobs, |g, _| many_passes(g, PASSES))
            .unwrap();
        let lists = frame.stats().unwrap().lists;
        let data: Vec<Vec<u32>> = tickets
            .into_iter()
            .map(|t| words(&frame.wait_readback(t).unwrap()))
            .collect();
        (lists, data)
    };
    let mut parallel = context();
    let mut serial = context();
    let (lists, on_jobs) = run(&mut parallel, Some(jobs()));
    let (_, on_one) = run(&mut serial, None);
    assert!(lists > 1, "{lists} lists");
    assert_eq!(on_jobs, on_one);
    for (i, data) in on_jobs.iter().enumerate() {
        assert_eq!(*data, expected(100 * i as u32), "pass {i}");
    }
    finish(parallel);
    finish(serial);
}

#[test]
fn a_compute_pass_splits_into_sub_lists() {
    const SUBS: usize = 4;
    let mut frame = context();
    let lists_before;
    let tickets = {
        let r = frame
            .execute_on(device(), Some(jobs()), |g, _| {
                let outs: Vec<BufferHandle<'_>> = (0..SUBS)
                    .map(|_| g.create_buffer("Out", BufferDesc::new(u64::from(WORDS) * 4)))
                    .collect();
                let outs = g.add_compute("Split", |b| {
                    let outs: Vec<_> = outs
                        .iter()
                        .map(|&o| b.write_buffer(o, Access::ComputeWrite))
                        .collect();
                    let mine = outs.clone();
                    (outs, move |ctx: &mut PassContext<'_>| {
                        ctx.parallel(SUBS, |i, sub| {
                            sub.set_pipeline(fill());
                            sub.push(&FillArgs {
                                output: sub.uav(mine[i]),
                                base: 7 * i as u32,
                            });
                            sub.dispatch(WORDS / 64, 1, 1);
                        });
                    })
                });
                outs.into_iter().map(|o| g.readback(o)).collect::<Vec<_>>()
            })
            .unwrap();
        lists_before = frame.stats().unwrap().lists;
        r
    };
    // The pass's own list, its four sub-lists and the list after them (and the readbacks').
    assert!(lists_before > SUBS + 1, "{lists_before} lists");
    for (i, t) in tickets.into_iter().enumerate() {
        assert_eq!(
            words(&frame.wait_readback(t).unwrap()),
            expected(7 * i as u32)
        );
    }
    finish(frame);
}

#[test]
fn a_raster_pass_splits_into_sub_lists_that_share_its_attachments() {
    static P: OnceLock<GraphicsPipeline> = OnceLock::new();
    let pipeline = P.get_or_init(|| {
        solid_pipeline(
            device(),
            "SolidPs",
            ColorTargetState::new(Format::Rgba8Unorm),
            |_| {},
        )
        .unwrap()
    });
    let mut frame = context();
    let r = frame
        .execute_on(device(), Some(jobs()), |g, _| {
            let target = g.create_texture("Target", TextureDesc::new_2d(4, 4, Format::Rgba8Unorm));
            let target = g.add_raster("Draw", |b| {
                b.record_in_parallel();
                let target = b.color(target, LoadOp::Clear([0.0, 0.0, 1.0, 1.0]), StoreOp::Store);
                (target, move |ctx: &mut PassContext<'_>| {
                    // The clear applies once, in the first sub-list; the second draws on top.
                    ctx.parallel(2, |i, sub| {
                        if i == 1 {
                            sub.set_pipeline(pipeline);
                            sub.push_words(&solid_constants(
                                [1.0, 0.0, 0.0, 1.0],
                                1,
                                0.5,
                                [0.0; 4],
                            ));
                            sub.draw(3, 1, 0, 0);
                        }
                    });
                })
            });
            g.readback_texture(target)
        })
        .unwrap();
    let pixels = frame.wait_readback(r).unwrap();
    let pixel = |x: usize, y: usize| &pixels[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
    assert_eq!(pixel(0, 3), [255, 0, 0, 255]);
    assert_eq!(pixel(1, 2), [255, 0, 0, 255]);
    assert_eq!(pixel(3, 0), [0, 0, 255, 255]);
    finish(frame);
}

#[test]
fn passes_report_their_gpu_time() {
    let mut frame = context();
    for _ in 0..4 {
        let tickets = frame.execute(device(), |g, _| many_passes(g, 2)).unwrap();
        for t in tickets {
            frame.wait_readback(t).unwrap();
        }
    }
    let names: Vec<&str> = frame.gpu_timings().iter().map(|t| t.name).collect();
    assert!(names.contains(&"Fill"), "{names:?}");
    assert!(names.contains(&"Readback"), "{names:?}");
    let dump = frame.dump().unwrap();
    assert!(dump.contains("\"gpu_ns\":"), "{dump}");
    finish(frame);
}

#[test]
fn a_capture_keeps_what_the_resource_held_at_that_point() {
    let mut frame = context();
    let desc = BufferDesc::new(u64::from(WORDS) * 4);
    frame
        .execute(device(), |g, _| {
            let out = g.create_buffer("Out", desc);
            let out = g.add_compute("Fill", |b| {
                let out = b.write_buffer(out, Access::ComputeWrite);
                (out, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(fill());
                    ctx.push(&FillArgs {
                        output: ctx.uav(out),
                        base: 9,
                    });
                    ctx.dispatch(WORDS / 64, 1, 1);
                })
            });
            g.capture(out, "FillDebug");
        })
        .unwrap();
    assert!(frame.captured_buffer("FillDebug").is_some());
    // A later frame reads the copy through the graph.
    let r = frame
        .execute(device(), |g, _| {
            let copy = g.persistent("FillDebug", CAPTURE_SCOPE, desc);
            assert!(!copy.fresh);
            g.readback(copy.handle)
        })
        .unwrap();
    assert_eq!(words(&frame.wait_readback(r).unwrap()), expected(9));
    finish(frame);
}
