//! Work spread over the compute and copy queues gives what one queue gives.

use std::sync::OnceLock;

use marlin_render_graph::{
    Access, BufferDesc, BufferHandle, FrameContext, FrameContextDesc, PassContext, RenderGraph,
};
use marlin_rhi::{ComputePipeline, QueueType};
use marlin_serial::cooked::Pod;

use crate::frame::{context, crate_compute, device, finish, words};

const WORDS: u32 = 256;

fn next() -> &'static ComputePipeline {
    static P: OnceLock<ComputePipeline> = OnceLock::new();
    P.get_or_init(|| crate_compute("RenderGraph.Next"))
}

fn fill() -> &'static ComputePipeline {
    static P: OnceLock<ComputePipeline> = OnceLock::new();
    P.get_or_init(|| marlin_gpu_test_support::compute_pipeline(device(), "Fill"))
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct NextArgs {
    input: u32,
    output: u32,
    has_input: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct FillArgs {
    output: u32,
    base: u32,
}

fn fill_pass<'g>(
    g: &mut RenderGraph<'g>,
    name: &str,
    asynchronous: bool,
    base: u32,
) -> BufferHandle<'g> {
    let out = g.create_buffer(name, BufferDesc::new(u64::from(WORDS) * 4));
    g.add_compute(name, |b| {
        if asynchronous {
            b.async_compute();
        }
        let out = b.write_buffer(out, Access::ComputeWrite);
        (out, move |ctx: &mut PassContext<'_>| {
            ctx.set_pipeline(fill());
            ctx.push(&FillArgs {
                output: ctx.uav(out),
                base,
            });
            ctx.dispatch(WORDS / 64, 1, 1);
        })
    })
}

fn next_pass<'g>(
    g: &mut RenderGraph<'g>,
    name: &str,
    asynchronous: bool,
    input: BufferHandle<'g>,
) -> BufferHandle<'g> {
    let out = g.create_buffer(name, BufferDesc::new(u64::from(WORDS) * 4));
    g.add_compute(name, |b| {
        if asynchronous {
            b.async_compute();
        }
        let input = b.read_buffer(input, Access::ComputeRead);
        let out = b.write_buffer(out, Access::ComputeWrite);
        (out, move |ctx: &mut PassContext<'_>| {
            ctx.set_pipeline(next());
            ctx.push(&NextArgs {
                input: ctx.srv(input),
                output: ctx.uav(out),
                has_input: 1,
            });
            ctx.dispatch(WORDS / 64, 1, 1);
        })
    })
}

/// Four frames of graphics → compute → graphics work with an independent async pass; the
/// readbacks of every frame.
fn run(frame: &mut FrameContext) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for f in 0..4u32 {
        let (a, b) = frame
            .execute(device(), |g, _| {
                let a = fill_pass(g, "Base", false, f * 1000);
                let lit = next_pass(g, "Light", true, a);
                let side = fill_pass(g, "Side", true, 7 + f);
                let composed = next_pass(g, "Compose", false, lit);
                (g.readback(composed), g.readback(side))
            })
            .unwrap();
        out.push(frame.wait_readback(a).unwrap());
        out.push(frame.wait_readback(b).unwrap());
    }
    out
}

#[test]
fn async_compute_gives_what_one_queue_gives() {
    let mut spread = context();
    let spread_out = run(&mut spread);
    if device().dedicated_queue(QueueType::Compute).is_some() {
        let plan = spread.cache().plan(spread.stats().unwrap().key).unwrap();
        assert!(
            plan.segments()
                .iter()
                .any(|s| s.queue == QueueType::Compute),
            "the async passes run on the compute queue"
        );
    }
    let mut single = FrameContext::new(
        device(),
        &FrameContextDesc {
            single_queue: true,
            ..FrameContextDesc::default()
        },
    )
    .unwrap();
    let single_out = run(&mut single);
    assert_eq!(spread_out, single_out);
    // And both are right: base + 3i, plus 2.
    let composed = words(&spread_out[6]);
    assert_eq!(composed[5], 3000 + 15 + 2);
    assert_eq!(words(&spread_out[7])[1], 10 + 3);
    finish(spread);
    finish(single);
}

#[test]
fn the_copy_queue_copies() {
    let mut frame = context();
    let r = frame
        .execute(device(), |g, _| {
            let a = fill_pass(g, "Source", false, 5);
            let b = g.create_buffer("Copy", BufferDesc::new(u64::from(WORDS) * 4));
            let b = g.add_copy("Upload", |p| {
                p.copy_queue();
                let a = p.read_buffer(a, Access::CopySrc);
                let b = p.write_buffer(b, Access::CopyDst);
                (b, move |ctx: &mut PassContext<'_>| {
                    ctx.copy_buffer(a, 0, b, 0, u64::from(WORDS) * 4)
                })
            });
            g.readback(b)
        })
        .unwrap();
    if device().dedicated_queue(QueueType::Copy).is_some() {
        let plan = frame.cache().plan(frame.stats().unwrap().key).unwrap();
        assert!(plan.segments().iter().any(|s| s.queue == QueueType::Copy));
    }
    let expected: Vec<u32> = (0..WORDS).map(|i| 5 + 3 * i).collect();
    assert_eq!(words(&frame.wait_readback(r).unwrap()), expected);
    finish(frame);
}
