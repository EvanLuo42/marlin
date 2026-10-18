//! The post chain: exposure follows the scene, adapts over frames, bloom's transients alias, the
//! histogram and exposure run on the compute queue.

use std::sync::OnceLock;

use marlin_gpu_test_support::{hardware_device, shader};
use marlin_render::{HDR_FORMAT, PostProcess, PostShaders};
use marlin_render_graph::{FrameContext, FrameContextDesc, PassContext, TextureDesc};
use marlin_rhi::{Format, HeapKind, LoadOp, QueueType, StoreOp};

const SIZE: u32 = 64;
const VIEW: u64 = 1;

fn post() -> &'static PostProcess {
    static POST: OnceLock<PostProcess> = OnceLock::new();
    POST.get_or_init(|| {
        PostProcess::new(
            hardware_device(),
            &PostShaders {
                bloom_down: shader("Marlin.Engine.Post.BloomDown"),
                bloom_up: shader("Marlin.Engine.Post.BloomUp"),
                histogram: shader("Marlin.Engine.Post.Histogram"),
                exposure: shader("Marlin.Engine.Post.Exposure"),
                fullscreen: shader("Marlin.Engine.Post.FullscreenVs"),
                tonemap: shader("Marlin.Engine.Post.TonemapPs"),
            },
            Format::Rgba8Unorm,
        )
        .unwrap()
    })
}

/// One frame of a uniformly grey HDR scene of luminance `luminance` through the chain; the
/// output's red channel at the centre.
fn frame_of(frame: &mut FrameContext, luminance: f32, dt: f32) -> u8 {
    let post = post();
    let ticket = frame
        .execute(hardware_device(), |g, _| {
            let scene = g.create_texture("Scene", TextureDesc::new_2d(SIZE, SIZE, HDR_FORMAT));
            let scene = g.add_raster("Scene", |b| {
                let s = b.color(
                    scene,
                    LoadOp::Clear([luminance, luminance, luminance, 1.0]),
                    StoreOp::Store,
                );
                (s, |_: &mut PassContext<'_>| {})
            });
            let output = g.create_texture(
                "Output",
                TextureDesc::new_2d(SIZE, SIZE, Format::Rgba8Unorm),
            );
            let out = post.add_passes(g, scene, output, VIEW, dt);
            g.readback_texture(out.output)
        })
        .unwrap();
    let pixels = frame.wait_readback(ticket).unwrap();
    let centre = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
    pixels[centre]
}

/// What the chain gives a uniform scene whose exposure has settled: exposure `1 / (1.2 * 2^EV100)`
/// with EV100 = log2(L * 100 / 12.5), so the exposed value is `(1 + bloom) / 9.6` whatever L is,
/// then the filmic curve.
fn settled() -> u8 {
    let x: f32 = (1.0 + post().settings.bloom_intensity) / 9.6;
    let y = (x * (2.51 * x + 0.03) / (x * (2.43 * x + 0.59) + 0.14)).clamp(0.0, 1.0);
    (y * 255.0).round() as u8
}

#[test]
fn exposure_brings_any_uniform_scene_to_the_same_grey() {
    let expected = settled();
    for luminance in [0.05, 1.0, 40.0] {
        // A fresh exposure takes the scene as it is.
        let mut frame = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
        let got = frame_of(&mut frame, luminance, 0.016);
        assert!(
            got.abs_diff(expected) <= 3,
            "luminance {luminance}: {got}, expected {expected}"
        );
        frame.wait_idle().unwrap();
    }
    assert_eq!(hardware_device().validation_errors(), 0);
}

#[test]
fn exposure_adapts_over_frames() {
    let mut frame = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let settled = settled();
    for _ in 0..3 {
        frame_of(&mut frame, 8.0, 0.016);
    }
    // The scene goes dark: the first frame after is too dark, then it brightens back.
    let first = frame_of(&mut frame, 0.25, 0.016);
    assert!(
        first + 10 < settled,
        "first dark frame {first}, settled {settled}"
    );
    let mut last = first;
    for _ in 0..200 {
        last = frame_of(&mut frame, 0.25, 0.05);
    }
    assert!(last > first, "{last} after {first}");
    assert!(
        last.abs_diff(settled) <= 3,
        "adapted {last}, settled {settled}"
    );
    frame.wait_idle().unwrap();
    assert_eq!(hardware_device().validation_errors(), 0);
}

#[test]
fn bloom_levels_share_memory_and_exposure_runs_on_the_compute_queue() {
    let mut frame = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    frame_of(&mut frame, 1.0, 0.016);
    let plan = frame.cache().plan(frame.stats().unwrap().key).unwrap();
    // Sum of the bloom chain's textures, against the heap they got.
    let device = hardware_device();
    let mut sum = 0;
    let mut bloom_textures = 0;
    for p in plan.passes() {
        if p.name.starts_with("Bloom") {
            bloom_textures += 1;
        }
    }
    for level in 0..post().settings.bloom_levels.min(SIZE.ilog2()) {
        let side = (SIZE >> (level + 1)).max(1);
        let req = device
            .texture_requirements(&marlin_rhi::TextureDesc::new_2d(
                side,
                side,
                HDR_FORMAT,
                marlin_rhi::TextureUsage::SAMPLED | marlin_rhi::TextureUsage::STORAGE,
            ))
            .unwrap();
        // A downsample and (but for the last level) an upsample per level.
        sum += req.size
            * if level + 1 == post().settings.bloom_levels.min(SIZE.ilog2()) {
                1
            } else {
                2
            };
    }
    assert!(bloom_textures > 4, "{bloom_textures} bloom passes");
    assert!(
        plan.heap_size(HeapKind::Textures) < sum,
        "the bloom chain aliases: heap {} < {sum}",
        plan.heap_size(HeapKind::Textures)
    );
    if device.dedicated_queue(QueueType::Compute).is_some() {
        for name in ["LuminanceClear", "LuminanceHistogram", "Exposure"] {
            let pass = plan.passes().iter().find(|p| p.name == name).unwrap();
            assert_eq!(pass.queue, QueueType::Compute, "{name}");
        }
    }
    frame.wait_idle().unwrap();
    assert_eq!(device.validation_errors(), 0);
}
