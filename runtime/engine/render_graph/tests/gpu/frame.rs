//! Frames on the GPU: aliasing, readbacks, uploads, raster passes, plan reuse and state carried
//! across frames.

use std::sync::OnceLock;

use marlin_gpu_test_support::{compute_pipeline, hardware_device, solid_constants, solid_pipeline};
use marlin_render_graph::{
    Access, BufferDesc, FrameContext, FrameContextDesc, Lookup, PassContext, RenderGraph,
    TextureDesc,
};
use marlin_rhi::{
    Buffer, BufferDesc as RhiBufferDesc, BufferUsage, ColorTargetState, ComputePipeline,
    ComputePipelineDesc, Device, Format, GraphicsPipeline, LoadOp, MemoryLocation, StoreOp,
};
use marlin_serial::cooked::Pod;

fn device() -> &'static Device {
    hardware_device()
}

fn context() -> FrameContext {
    FrameContext::new(device(), &FrameContextDesc::default()).unwrap()
}

/// Ends a test: the GPU is done and the validation layer had nothing to say.
fn finish(mut frame: FrameContext) {
    frame.wait_idle().unwrap();
    drop(frame);
    assert_eq!(device().validation_errors(), 0);
}

/// `Test.Fill`: word i = base + 3i, 64 threads a group.
fn fill() -> &'static ComputePipeline {
    static P: OnceLock<ComputePipeline> = OnceLock::new();
    P.get_or_init(|| compute_pipeline(device(), "Fill"))
}

fn crate_compute(entry: &'static str) -> ComputePipeline {
    device()
        .create_compute_pipeline(&ComputePipelineDesc {
            shader: marlin_gpu_test_support::Shader::new(entry)
                .in_crate(env!("CARGO_MANIFEST_DIR"))
                .module(),
            cache: None,
        })
        .unwrap()
}

fn fill_texture() -> &'static ComputePipeline {
    static P: OnceLock<ComputePipeline> = OnceLock::new();
    P.get_or_init(|| crate_compute("RenderGraph.FillTexture"))
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_ne_bytes(*c))
        .collect()
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct FillArgs {
    output: u32,
    base: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct FillTextureArgs {
    output: u32,
    base: u32,
    size: u32,
}

const WORDS: u64 = 256;

/// A compute pass filling `buffer` (WORDS words) with `Test.Fill` from `base`.
fn fill_pass<'g>(
    g: &mut RenderGraph<'g>,
    name: &str,
    buffer: marlin_render_graph::BufferHandle<'g>,
    base: u32,
) -> marlin_render_graph::BufferHandle<'g> {
    g.add_compute(name, |b| {
        let out = b.write_buffer(buffer, Access::ComputeWrite);
        (out, move |ctx: &mut PassContext<'_>| {
            ctx.set_pipeline(fill());
            ctx.push(&FillArgs {
                output: ctx.uav(out),
                base,
            });
            ctx.dispatch(WORDS as u32 / 64, 1, 1);
        })
    })
}

fn expected_fill(base: u32) -> Vec<u32> {
    (0..WORDS as u32).map(|i| base + 3 * i).collect()
}

#[test]
fn aliased_buffers_each_keep_what_was_written() {
    let mut frame = context();
    let (a, b) = frame
        .execute(device(), |g, _| {
            let a = g.create_buffer("A", BufferDesc::new(WORDS * 4));
            let a = fill_pass(g, "FillA", a, 100);
            let ra = g.readback(a);
            let b = g.create_buffer("B", BufferDesc::new(WORDS * 4));
            let b = fill_pass(g, "FillB", b, 7);
            let rb = g.readback(b);
            (ra, rb)
        })
        .unwrap();
    let plan = frame.cache().plan(frame.stats().unwrap().key).unwrap();
    // A's last use (its readback) ends before B's first: one buffer's worth of memory for both.
    let one = device().buffer_requirements(&RhiBufferDesc {
        size: WORDS * 4,
        usage: BufferUsage::SHADER_WRITE | BufferUsage::COPY_SRC,
        memory: MemoryLocation::GpuOnly,
    });
    assert_eq!(plan.heap_size(marlin_rhi::HeapKind::Buffers), one.size);
    assert_eq!(words(&frame.wait_readback(a).unwrap()), expected_fill(100));
    assert_eq!(words(&frame.wait_readback(b).unwrap()), expected_fill(7));
    finish(frame);
}

#[test]
fn aliased_textures_each_keep_what_was_written() {
    const SIZE: u32 = 16;
    let mut frame = context();
    let fill = |g: &mut RenderGraph<'_>, name: &str, base: u32| {
        let t = g.create_texture(name, TextureDesc::new_2d(SIZE, SIZE, Format::R32Uint));
        let t = g.add_compute(name, |b| {
            let out = b.write_texture(t, Access::ComputeWrite);
            (out, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(fill_texture());
                ctx.push(&FillTextureArgs {
                    output: ctx.uav(out),
                    base,
                    size: SIZE,
                });
                ctx.dispatch(SIZE / 8, SIZE / 8, 1);
            })
        });
        g.readback_texture(t)
    };
    let (a, b) = frame
        .execute(device(), |g, _| (fill(g, "A", 1000), fill(g, "B", 5)))
        .unwrap();
    let plan = frame.cache().plan(frame.stats().unwrap().key).unwrap();
    // One texture's worth of memory for both.
    let one = device()
        .texture_requirements(&marlin_rhi::TextureDesc::new_2d(
            SIZE,
            SIZE,
            Format::R32Uint,
            marlin_rhi::TextureUsage::STORAGE | marlin_rhi::TextureUsage::COPY_SRC,
        ))
        .unwrap();
    assert_eq!(plan.heap_size(marlin_rhi::HeapKind::Textures), one.size);
    let expected = |base: u32| (0..SIZE * SIZE).map(|i| base + i).collect::<Vec<_>>();
    assert_eq!(words(&frame.wait_readback(a).unwrap()), expected(1000));
    assert_eq!(words(&frame.wait_readback(b).unwrap()), expected(5));
    finish(frame);
}

#[test]
fn a_steady_frame_reuses_its_plan_and_creates_nothing() {
    let mut frame = context();
    for i in 0..4 {
        let r = frame
            .execute(device(), |g, _| {
                let a = g.create_buffer("A", BufferDesc::new(WORDS * 4));
                let a = fill_pass(g, "Fill", a, i);
                g.readback(a)
            })
            .unwrap();
        let stats = frame.stats().unwrap();
        assert_eq!(
            stats.lookup,
            if i == 0 { Lookup::Miss } else { Lookup::Hit },
            "frame {i}"
        );
        // Each frame slot builds the plan's resources once.
        if i >= 2 {
            assert_eq!(stats.created, 0, "frame {i}");
        } else {
            assert!(stats.created > 0, "frame {i}");
        }
        assert_eq!(words(&frame.wait_readback(r).unwrap()), expected_fill(i));
    }
    finish(frame);
}

fn readable_buffer(words: u64) -> Buffer {
    let b = device()
        .create_buffer(&RhiBufferDesc {
            size: words * 4,
            usage: BufferUsage::SHADER_WRITE | BufferUsage::SHADER_READ | BufferUsage::COPY_SRC,
            memory: MemoryLocation::GpuOnly,
        })
        .unwrap();
    b.set_name("Persistent");
    b
}

#[test]
fn an_import_keeps_its_contents_and_state_across_frames() {
    let mut frame = context();
    let persistent = readable_buffer(WORDS);
    frame
        .execute(device(), |g, _| {
            let b = g.import_buffer("Persistent", &persistent);
            fill_pass(g, "Fill", b, 42);
        })
        .unwrap();
    // The next frame finds it written by compute shaders, without being told.
    let r = frame
        .execute(device(), |g, _| {
            let b = g.import_buffer("Persistent", &persistent);
            g.readback(b)
        })
        .unwrap();
    assert_eq!(words(&frame.wait_readback(r).unwrap()), expected_fill(42));
    frame.forget_buffer(&persistent);
    finish(frame);
}

#[test]
fn uploads_reach_the_shaders() {
    #[repr(C)]
    #[derive(Pod, Clone, Copy)]
    struct CopyArgs {
        input: u32,
        output: u32,
    }
    #[repr(C)]
    #[derive(Pod, Clone, Copy)]
    struct Values {
        words: [u32; 4],
    }
    static P: OnceLock<ComputePipeline> = OnceLock::new();
    let pipeline = P.get_or_init(|| crate_compute("RenderGraph.CopyUniform"));
    let mut frame = context();
    let r = frame
        .execute(device(), |g, _| {
            let out = g.create_buffer("Out", BufferDesc::new(16));
            let out = g.add_compute("CopyUniform", |b| {
                let out = b.write_buffer(out, Access::ComputeWrite);
                (out, move |ctx: &mut PassContext<'_>| {
                    let input = ctx.upload(&Values {
                        words: [11, 22, 33, 44],
                    });
                    ctx.set_pipeline(pipeline);
                    ctx.push(&CopyArgs {
                        input,
                        output: ctx.uav(out),
                    });
                    ctx.dispatch(1, 1, 1);
                })
            });
            g.readback(out)
        })
        .unwrap();
    assert_eq!(words(&frame.wait_readback(r).unwrap()), [11, 22, 33, 44]);
    finish(frame);
}

#[test]
fn a_raster_pass_draws_into_a_transient_target() {
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
        .execute(device(), |g, _| {
            let target = g.create_texture("Target", TextureDesc::new_2d(4, 4, Format::Rgba8Unorm));
            let target = g.add_raster("Draw", |b| {
                let target = b.color(target, LoadOp::Clear([0.0, 0.0, 1.0, 1.0]), StoreOp::Store);
                (target, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(pipeline);
                    // Mode 1: the lower-left half below the diagonal.
                    ctx.push_words(&solid_constants([1.0, 0.0, 0.0, 1.0], 1, 0.5, [0.0; 4]));
                    ctx.draw(3, 1, 0, 0);
                })
            });
            g.readback_texture(target)
        })
        .unwrap();
    let pixels = frame.wait_readback(r).unwrap();
    assert_eq!(pixels.len(), 4 * 4 * 4);
    let pixel = |x: usize, y: usize| &pixels[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
    // Below the diagonal drawn red, above it still cleared to blue (pixels on it left out).
    assert_eq!(pixel(0, 3), [255, 0, 0, 255]);
    assert_eq!(pixel(1, 2), [255, 0, 0, 255]);
    assert_eq!(pixel(3, 0), [0, 0, 255, 255]);
    assert_eq!(pixel(2, 1), [0, 0, 255, 255]);
    finish(frame);
}
