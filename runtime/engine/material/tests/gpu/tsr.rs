//! TSR (0010 §7.1) on the edges scene, against a supersampled reference: the same view drawn at
//! 4× the size without jitter and box-filtered down (4×4 samples a pixel).

use marlin_gpu_test_support::{Image, assert_image};
use marlin_render::upscale::UpscaleMode;
use marlin_test_scenes::scenes::edges;

use crate::support::Harness;

const W: u32 = 128;
const H: u32 = 80;

/// The view at `W` × `H`, supersampled 4×4.
fn reference(h: &mut Harness) -> Vec<[f32; 3]> {
    let big = h.upscaled_frame(0, None, (4 * W, 4 * H));
    let mut out = vec![[0.0f32; 3]; (W * H) as usize];
    for y in 0..4 * H {
        for x in 0..4 * W {
            let p = big[(y * 4 * W + x) as usize];
            let o = &mut out[((y / 4) * W + x / 4) as usize];
            for i in 0..3 {
                o[i] += p[i] / 16.0;
            }
        }
    }
    out
}

/// The mean difference of two pictures, compressed as a display would (`c / (1 + c)`), so
/// bright and dark edges count alike.
fn difference(a: &[[f32; 3]], b: &[[f32; 3]]) -> f32 {
    let t = |c: f32| c / (1.0 + c);
    let sum: f32 = a
        .iter()
        .zip(b)
        .map(|(p, q)| (0..3).map(|i| (t(p[i]) - t(q[i])).abs()).sum::<f32>() / 3.0)
        .sum();
    sum / a.len() as f32
}

fn harness() -> Harness {
    let mut h = Harness::new();
    h.load(&edges());
    h
}

/// `small` (`w` × `h`) stretched to `W` × `H`, bilinear.
fn bilinear(small: &[[f32; 3]], w: u32, h: u32) -> Vec<[f32; 3]> {
    let at = |x: i32, y: i32| {
        small[(y.clamp(0, h as i32 - 1) as u32 * w + x.clamp(0, w as i32 - 1) as u32) as usize]
    };
    let mut out = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let fx = (x as f32 + 0.5) * w as f32 / W as f32 - 0.5;
            let fy = (y as f32 + 0.5) * h as f32 / H as f32 - 0.5;
            let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
            let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
            let c: [f32; 3] = core::array::from_fn(|i| {
                let top = at(x0, y0)[i] * (1.0 - tx) + at(x0 + 1, y0)[i] * tx;
                let bottom = at(x0, y0 + 1)[i] * (1.0 - tx) + at(x0 + 1, y0 + 1)[i] * tx;
                top * (1.0 - ty) + bottom * ty
            });
            out.push(c);
        }
    }
    out
}

/// TSR's frame `frames` of the edges view in `mode`.
fn tsr_after(mode: UpscaleMode, frames: u64) -> Vec<[f32; 3]> {
    let mut h = harness();
    let mut frame = Vec::new();
    for i in 0..frames {
        frame = h.upscaled_frame(i, Some(mode), (W, H));
    }
    frame
}

#[test]
fn tsr_anti_aliases_toward_supersampling() {
    // Measured: 0.0022 against 0.0048 for the aliased frame.
    let truth = reference(&mut harness());
    let aliased = difference(&harness().upscaled_frame(0, None, (W, H)), &truth);
    let tsr = difference(&tsr_after(UpscaleMode::Native, 64), &truth);
    eprintln!("against 4×4 supersampling: aliased {aliased:.5}, TSR {tsr:.5}");
    assert!(tsr < 0.6 * aliased, "TSR {tsr}, aliased {aliased}");
}

#[test]
fn upscaling_beats_stretching() {
    // Drawn at 1/1.5 and 1/2 the size: TSR against the small frame stretched bilinearly
    // (measured: 0.0038 and 0.0074 at 1.5×, 0.0045 and 0.0130 at 2×; the native aliased frame
    // is 0.0048).
    use marlin_render::upscale::Upscaler;
    let truth = reference(&mut harness());
    for mode in [UpscaleMode::Quality, UpscaleMode::Performance] {
        let r = harness()
            .tsr
            .render_size(marlin_math::UVec2::new(W, H), mode);
        let tsr = difference(&tsr_after(mode, 64), &truth);
        let small = harness().upscaled_frame(0, None, (r.x, r.y));
        let stretched = difference(&bilinear(&small, r.x, r.y), &truth);
        eprintln!(
            "{mode:?} ({} × {}): TSR {tsr:.5}, bilinear {stretched:.5}",
            r.x, r.y
        );
        assert!(
            tsr < 0.7 * stretched,
            "{mode:?}: TSR {tsr}, bilinear {stretched}"
        );
    }
}

#[test]
fn a_pan_keeps_the_picture_in_place() {
    // 32 frames moving 2 cm a frame sideways (0.4 pixels): the history follows the motion
    // vectors, so the frame stays better than an aliased one of the same view (measured: 0.0042
    // against 0.0049); misplaced, it would smear. Resampling a moving history softens it, which
    // is what is left of the static 0.0022.
    let mut h = harness();
    for frame in 0..48u64 {
        if frame >= 16 {
            h.camera.0.translation.x += 0.02;
        }
        h.upscaled_frame(frame, Some(UpscaleMode::Native), (W, H));
    }
    let tsr = h.upscaled_frame(48, Some(UpscaleMode::Native), (W, H));
    let mut there = harness();
    there.camera = h.camera;
    there.camera.1.jitter = [0.0; 2];
    let truth = reference(&mut there);
    let aliased = difference(&there.upscaled_frame(0, None, (W, H)), &truth);
    let tsr = difference(&tsr, &truth);
    eprintln!("after the pan: TSR {tsr:.5}, aliased {aliased:.5}");
    assert!(tsr < 0.95 * aliased, "TSR {tsr}, aliased {aliased}");
}

#[test]
fn the_jitter_covers_the_pixel_evenly() {
    use marlin_render::upscale::{Tsr, Upscaler};
    let h = harness();
    for (ratio, render) in [(1.0f32, 64u32), (2.0, 32)] {
        let display = marlin_math::UVec2::new(64, 64);
        let render = marlin_math::UVec2::new(render, render);
        let phases = Tsr::phases(ratio);
        let jitters: Vec<_> = (0..phases)
            .map(|f| h.tsr.jitter(f, render, display))
            .collect();
        // Halton (2, 3): inside the pixel, centered, and every quadrant visited alike.
        let n = phases as f32;
        let mean = jitters
            .iter()
            .fold((0.0, 0.0), |m, j| (m.0 + j.x / n, m.1 + j.y / n));
        assert!(jitters.iter().all(|j| j.x.abs() < 0.5 && j.y.abs() < 0.5));
        assert!(
            mean.0.abs() < 0.07 && mean.1.abs() < 0.07,
            "{ratio}×: mean {mean:?}"
        );
        for q in 0..4 {
            let inside = jitters
                .iter()
                .filter(|j| (j.x >= 0.0) == (q & 1 == 1) && (j.y >= 0.0) == (q & 2 == 2))
                .count();
            assert!(
                (inside as f32 - n / 4.0).abs() <= n / 8.0 + 1.0,
                "{ratio}×: quadrant {q} has {inside} of {phases}"
            );
        }
        // The pattern repeats after its phases.
        assert_eq!(h.tsr.jitter(phases, render, display), jitters[0]);
    }
    assert_eq!(
        (Tsr::phases(1.0), Tsr::phases(2.0), Tsr::phases(3.0)),
        (8, 32, 32)
    );
}

#[test]
fn the_upscaled_edges_look_as_they_did() {
    let frame = tsr_after(UpscaleMode::Performance, 64);
    let pixels: Vec<u8> = frame
        .iter()
        .flat_map(|p| {
            let [r, g, b] = p.map(|c| ((c / (1.0 + c)).powf(1.0 / 2.2) * 255.0).round() as u8);
            [r, g, b, 255]
        })
        .collect();
    assert_image!("edges_tsr_performance", &Image::new(W, H, pixels));
}

#[test]
fn tsr_costs_little() {
    // Resolving to 1080p (measured: 0.17 ms on an RTX 5060, native or from half the size); a
    // budget, not a benchmark.
    for mode in [UpscaleMode::Native, UpscaleMode::Performance] {
        let mut h = harness();
        let mut times = Vec::new();
        for frame in 0..16u64 {
            h.upscaled_frame(frame, Some(mode), (1920, 1080));
            if frame >= 4 {
                let t: f64 = h
                    .pass_times()
                    .iter()
                    .filter(|(n, _)| n.starts_with("Tsr"))
                    .map(|(_, t)| t)
                    .sum();
                times.push(t);
            }
        }
        times.sort_by(f64::total_cmp);
        let median = times[times.len() / 2];
        eprintln!("TSR {mode:?} to 1920 × 1080: {median:.3} ms");
        assert!(median > 0.0 && median < 0.5, "{mode:?}: {median:.3} ms");
    }
}

#[test]
fn a_reactive_mask_takes_the_history_away() {
    // The whole view reactive: TSR keeps no history, so after 64 frames it is as aliased as one
    // frame, worse for the jitter (measured: 0.0062 against 0.0048 aliased); half reactive, it
    // still anti-aliases, less (0.0030 against 0.0023 without the mask).
    let truth = reference(&mut harness());
    let aliased = difference(&harness().upscaled_frame(0, None, (W, H)), &truth);
    let after = |reactive: f32| {
        let mut h = harness();
        h.uniform_reactive = Some(reactive);
        let mut frame = Vec::new();
        for i in 0..64 {
            frame = h.upscaled_frame(i, Some(UpscaleMode::Native), (W, H));
        }
        difference(&frame, &truth)
    };
    let (none, half, full) = (after(0.0), after(0.5), after(1.0));
    eprintln!("reactive 0: {none:.5}, ½: {half:.5}, 1: {full:.5}; aliased {aliased:.5}");
    assert!(none < 0.6 * aliased, "{none} against {aliased}");
    assert!(none < half && half < full, "{none}, {half}, {full}");
    assert!(full > 0.8 * aliased, "{full} against {aliased}");
}
