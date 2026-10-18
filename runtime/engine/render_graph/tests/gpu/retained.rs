//! History and persistent resources on the GPU: what one frame writes, the next reads.

use std::sync::OnceLock;

use marlin_render_graph::{Access, BufferDesc, GLOBAL_SCOPE, PassContext};
use marlin_rhi::ComputePipeline;
use marlin_serial::cooked::Pod;

use crate::frame::{context, crate_compute, device, finish, words};

const WORDS: u32 = 128;

fn next() -> &'static ComputePipeline {
    static P: OnceLock<ComputePipeline> = OnceLock::new();
    P.get_or_init(|| crate_compute("RenderGraph.Next"))
}

fn increment() -> &'static ComputePipeline {
    static P: OnceLock<ComputePipeline> = OnceLock::new();
    P.get_or_init(|| crate_compute("RenderGraph.Increment"))
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct NextArgs {
    input: u32,
    output: u32,
    has_input: u32,
}

#[test]
fn each_frame_reads_what_the_last_wrote() {
    let mut frame = context();
    for f in 0..6u32 {
        let r = frame
            .execute(device(), |g, _| {
                let h = g.history("Counts", 1, BufferDesc::new(u64::from(WORDS) * 4));
                let cur = g.add_compute("Next", |b| {
                    let prev = h.prev.map(|p| b.read_buffer(p, Access::ComputeRead));
                    let cur = b.write_buffer(h.cur, Access::ComputeWrite);
                    (cur, move |ctx: &mut PassContext<'_>| {
                        ctx.set_pipeline(next());
                        ctx.push(&NextArgs {
                            input: prev.map_or(0, |p| ctx.srv(p)),
                            output: ctx.uav(cur),
                            has_input: u32::from(prev.is_some()),
                        });
                        ctx.dispatch(WORDS / 64, 1, 1);
                    })
                });
                g.readback(cur)
            })
            .unwrap();
        assert_eq!(
            words(&frame.wait_readback(r).unwrap()),
            vec![f + 1; WORDS as usize],
            "frame {f}"
        );
        // Both objects and their views exist after the third frame.
        if f >= 3 {
            assert_eq!(frame.stats().unwrap().created, 0, "frame {f}");
        }
    }
    finish(frame);
}

#[test]
fn a_persistent_buffer_accumulates_and_starts_over_when_resized() {
    let mut frame = context();
    let run = |frame: &mut marlin_render_graph::FrameContext, words_len: u32| {
        frame
            .execute(device(), |g, _| {
                let p = g.persistent(
                    "Total",
                    GLOBAL_SCOPE,
                    BufferDesc::new(u64::from(words_len) * 4),
                );
                let mut data = p.handle;
                if p.fresh {
                    data = g.add_copy("Clear", |b| {
                        let data = b.write_buffer(data, Access::Clear);
                        (data, move |ctx: &mut PassContext<'_>| {
                            ctx.fill_buffer(data, 0)
                        })
                    });
                }
                let data = g.add_compute("Increment", |b| {
                    let data = b.write_buffer(data, Access::ComputeWrite);
                    (data, move |ctx: &mut PassContext<'_>| {
                        ctx.set_pipeline(increment());
                        ctx.push_words(&[ctx.uav(data)]);
                        ctx.dispatch(words_len / 64, 1, 1);
                    })
                });
                g.readback(data)
            })
            .unwrap()
    };
    let mut seen = Vec::new();
    for len in [WORDS, WORDS, WORDS, 2 * WORDS, 2 * WORDS] {
        let r = run(&mut frame, len);
        let w = words(&frame.wait_readback(r).unwrap());
        assert!(w.iter().all(|&v| v == w[0]), "{w:?}");
        assert_eq!(w.len(), len as usize);
        seen.push(w[0]);
    }
    assert_eq!(seen, [1, 2, 3, 1, 2]);
    finish(frame);
}
