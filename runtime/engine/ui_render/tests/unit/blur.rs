//! Backdrop blurs (§5.6): how a frame is cut into layers, each blur's image, and tile lists that
//! keep what a blur reads.

use marlin_font::GlyphIds;
use marlin_ui_render::blur::{BlurJob, Rgba8, blur, plan};
use marlin_ui_render::command::Chunk;
use marlin_ui_render::{Color, Corners, Painter, Rect, RectStyle, UiDrawList, reference};

fn record(f: impl FnOnce(&mut Painter<'_>)) -> Chunk {
    let ids = GlyphIds::new();
    let mut list = UiDrawList::new(1.0);
    f(&mut list.painter(&ids));
    list.flatten()
}

fn background(p: &mut Painter<'_>) {
    p.rect(
        Rect::new(0.0, 0.0, 400.0, 200.0),
        &RectStyle::fill(Color::rgb(30, 60, 90)),
    );
}

#[test]
fn blurs_start_layers_only_over_what_was_drawn_since() {
    let none = record(background);
    let p = plan(&none, &[], 400, 200);
    assert_eq!((p.starts.as_slice(), p.jobs.len()), (&[0][..], 0));

    let chunk = record(|p| {
        background(p);
        // Two panels apart: one layer. A third over the first panel's content: a new one.
        p.backdrop_blur(Rect::new(10.0, 10.0, 110.0, 90.0), 8.0, Corners::all(8.0));
        p.rect(
            Rect::new(20.0, 20.0, 100.0, 40.0),
            &RectStyle::fill(Color::WHITE),
        );
        p.backdrop_blur(Rect::new(200.0, 10.0, 300.0, 90.0), 8.0, Corners::ZERO);
        p.backdrop_blur(Rect::new(60.0, 30.0, 160.0, 120.0), 8.0, Corners::ZERO);
    });
    let p = plan(&chunk, &[], 400, 200);
    assert_eq!(p.starts, [0, 1, 4]);
    let layers: Vec<u32> = p.jobs.iter().map(|j| j.layer).collect();
    assert_eq!(layers, [1, 1, 2]);
    assert_eq!(p.layer_of(0), 0);
    assert_eq!(p.layer_of(3), 1);
    assert_eq!(p.range(1, 5), (1, 4));
    assert_eq!(p.range(2, 5), (4, 5));
}

#[test]
fn large_blurs_are_filtered_scaled_down() {
    let chunk = record(|p| {
        background(p);
        p.backdrop_blur(Rect::new(100.0, 50.0, 200.0, 150.0), 40.0, Corners::ZERO);
    });
    let j = plan(&chunk, &[], 400, 200).jobs[0];
    // σ 20 at an eighth: 2.5 texels; the region is the box grown by 3σ, from a multiple of 8.
    assert_eq!((j.scale, j.sigma), (8, 2.5));
    assert_eq!(j.origin, [32, 0]);
    // 100 - 1 - 60 = 39 → 32; 200 + 1 + 60 = 261: (261 - 32) / 8 → 29 texels; 150 + 61 → 200.
    assert_eq!(j.size, [29, 25]);
}

#[test]
fn the_blur_keeps_flat_color_and_spreads_an_edge() {
    let (w, h) = (64u32, 8u32);
    let mut texels = Vec::new();
    for _ in 0..h {
        for x in 0..w {
            let v = if x < 32 { 0 } else { 255 };
            texels.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let src = Rgba8 {
        width: w,
        height: h,
        texels,
    };
    let job = BlurJob {
        command: 0,
        layer: 1,
        origin: [0, 0],
        size: [w, h],
        scale: 1,
        sigma: 3.0,
    };
    let out = blur(&src, &job);
    let at = |x: i32| out.load(x, 4)[0];
    assert_eq!(out.load(0, 0), [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(out.load(63, 7), [1.0; 4]);
    assert!(
        (at(31) + at(32) - 1.0).abs() < 2.0 / 255.0,
        "{} {}",
        at(31),
        at(32)
    );
    assert!((0..63).all(|x| at(x) <= at(x + 1)));
    assert!(at(28) > 0.0 && at(35) < 1.0);
}

#[test]
fn opaque_commands_keep_what_a_later_layer_reads() {
    // An opaque panel after the blur covers whole tiles; the background under it stays listed,
    // since the blur's image is made from it.
    let chunk = record(|p| {
        background(p);
        p.backdrop_blur(Rect::new(0.0, 0.0, 64.0, 64.0), 4.0, Corners::ZERO);
        p.rect(
            Rect::new(64.0, 0.0, 128.0, 64.0),
            &RectStyle::fill(Color::WHITE),
        );
        p.rect(
            Rect::new(64.0, 0.0, 128.0, 64.0),
            &RectStyle::fill(Color::BLACK),
        );
    });
    let tiles = reference::bin(&chunk, &[], 400, 200);
    let (tx, _) = reference::tile_counts(400, 200);
    // Tile (4, 1): the background, then the black panel; the white one is in the same layer
    // and hidden.
    assert_eq!(tiles[(tx + 4) as usize].commands, [0, 3]);
    // Without the blur, the background goes too.
    let flat = record(|p| {
        background(p);
        p.rect(
            Rect::new(64.0, 0.0, 128.0, 64.0),
            &RectStyle::fill(Color::BLACK),
        );
    });
    assert_eq!(
        reference::bin(&flat, &[], 400, 200)[(tx + 4) as usize].commands,
        [1]
    );
}
