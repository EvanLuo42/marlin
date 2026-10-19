//! Path fill on the GPU (U2c): tile lists and pixels against the reference, and a golden image.

use marlin_gpu_test_support::{assert_image, hardware_device};
use marlin_math::Vec2;
use marlin_render_graph::{FrameContext, FrameContextDesc};
use marlin_ui_render::path;
use marlin_ui_render::{
    Affine2, Color, Corners, FillRule, GroupDesc, Paint, Painter, Path, Rect, RectStyle,
    UiDrawList, UiRendererDesc,
};

use crate::binning::compare;
use crate::shading::{close, draw};
use crate::support::Pages;
use crate::{finish, renderer};

fn star(c: Vec2, r: f32) -> Path {
    let mut p = Path::new();
    for i in 0..5 {
        let a = -core::f32::consts::FRAC_PI_2 + i as f32 * 4.0 * core::f32::consts::PI / 5.0;
        let q = c + Vec2::new(a.cos(), a.sin()) * r;
        if i == 0 {
            p.move_to(q);
        } else {
            p.line_to(q);
        }
    }
    p.close();
    p
}

/// A heart of cubics, a ring of quadratics with a hole, stars by both rules, a tall sliver and
/// a turned, clipped icon.
fn scene(p: &mut Painter<'_>) {
    p.rect(
        Rect::new(0.0, 0.0, 320.0, 200.0),
        &RectStyle::fill(Color::rgb(20, 22, 28)),
    );
    let mut heart = Path::new();
    heart
        .move_to(Vec2::new(60.0, 60.0))
        .cubic_to(
            Vec2::new(60.0, 40.0),
            Vec2::new(25.0, 35.0),
            Vec2::new(25.0, 62.0),
        )
        .cubic_to(
            Vec2::new(25.0, 85.0),
            Vec2::new(55.0, 95.0),
            Vec2::new(60.0, 110.0),
        )
        .cubic_to(
            Vec2::new(65.0, 95.0),
            Vec2::new(95.0, 85.0),
            Vec2::new(95.0, 62.0),
        )
        .cubic_to(
            Vec2::new(95.0, 35.0),
            Vec2::new(60.0, 40.0),
            Vec2::new(60.0, 60.0),
        )
        .close();
    p.fill_path(
        &heart,
        FillRule::NonZero,
        &Paint::Solid(Color::rgb(230, 60, 80)),
    );
    let mut ring = Path::new();
    for (r, cw) in [(34.0f32, true), (20.0, false)] {
        let c = Vec2::new(160.0, 70.0);
        let pts: Vec<Vec2> = (0..8)
            .map(|i| {
                let a = i as f32 * core::f32::consts::FRAC_PI_4 * if cw { 1.0 } else { -1.0 };
                c + Vec2::new(a.cos(), a.sin()) * r
            })
            .collect();
        ring.move_to(pts[0]);
        for i in 0..8 {
            // Control points on the circle's tangents (a coarse quadratic circle).
            let (a, b) = (pts[i], pts[(i + 1) % 8]);
            let mid = (a + b) * 0.5;
            let ctrl = c + (mid - c) * (r / (mid - c).length() * 1.08);
            ring.quad_to(ctrl, b);
        }
        ring.close();
    }
    p.fill_path(
        &ring,
        FillRule::NonZero,
        &Paint::Solid(Color::rgb(80, 200, 140)),
    );
    p.fill_path(
        &star(Vec2::new(250.0, 60.0), 40.0),
        FillRule::NonZero,
        &Paint::Solid(Color::rgb(250, 210, 60)),
    );
    p.fill_path(
        &star(Vec2::new(250.0, 150.0), 40.0),
        FillRule::EvenOdd,
        &Paint::Solid(Color::rgb(250, 210, 60)),
    );
    let mut sliver = Path::new();
    sliver
        .move_to(Vec2::new(110.0, 2.0))
        .line_to(Vec2::new(113.0, 2.0))
        .line_to(Vec2::new(140.0, 198.0))
        .line_to(Vec2::new(137.0, 198.0))
        .close();
    p.fill_path(
        &sliver,
        FillRule::NonZero,
        &Paint::Solid(Color::rgba(255, 255, 255, 160)),
    );
    p.push_clip(Rect::new(20.0, 120.0, 100.0, 190.0), Corners::all(12.0));
    p.begin_group(&GroupDesc {
        transform: Affine2::translate(60.0, 155.0)
            * Affine2::rotate(0.5)
            * Affine2::scale(1.5, 1.5),
        opacity: 0.8,
        ..GroupDesc::default()
    });
    p.fill_path(
        &star(Vec2::ZERO, 30.0),
        FillRule::EvenOdd,
        &Paint::Solid(Color::rgb(90, 150, 255)),
    );
    p.end_group();
    p.pop_clip();
}

#[test]
fn paths_bin_and_shade_like_the_reference() {
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let mut pages = Pages::new(r.glyph_cache().config().page_size);
    let mut list = UiDrawList::new(1.0);
    scene(&mut list.painter(&r.glyph_cache().ids));
    let (gpu, cpu) = draw(
        &mut r,
        &mut frames,
        &mut pages,
        &list,
        320,
        200,
        Color::BLACK,
    );
    close(&gpu, &cpu, 3, 0.01);
    compare(&mut r, &mut frames, &list, 320, 200);
    assert_image!("ui_paths", &gpu);
    finish(&mut frames, &r);
}

/// Text from its outlines: a large title, turned text, and zooming text in a gradient.
#[test]
fn text_as_paths_shades_like_the_reference() {
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let mut pages = Pages::new(r.glyph_cache().config().page_size);
    let font = crate::support::inter();
    let fonts = [font];
    let mut list = UiDrawList::new(1.0);
    {
        let mut p = list.painter(&r.glyph_cache().ids).with_fonts(&fonts);
        p.rect(
            Rect::new(0.0, 0.0, 320.0, 200.0),
            &RectStyle::fill(Color::rgb(245, 245, 240)),
        );
        let title = marlin_font::TextLayout::new(&font, marlin_font::FontId(0), 96.0, "Ag");
        p.text(&title, Vec2::new(8.0, -10.0), Color::rgb(30, 30, 40));
        let turned =
            marlin_font::TextLayout::new(&font, marlin_font::FontId(0), 18.0, "turned text");
        p.begin_group(&GroupDesc {
            transform: Affine2::translate(170.0, 40.0) * Affine2::rotate(0.5),
            ..GroupDesc::default()
        });
        p.text(&turned, Vec2::ZERO, Color::rgb(200, 40, 60));
        p.end_group();
        p.set_text_as_paths(true);
        let zoom = marlin_font::TextLayout::new(&font, marlin_font::FontId(0), 24.0, "zooming");
        let g = marlin_ui_render::Gradient::linear(
            Vec2::new(150.0, 0.0),
            Vec2::new(300.0, 0.0),
            &[
                (0.0, Color::rgb(40, 90, 255)),
                (1.0, Color::rgb(255, 120, 40)),
            ],
        );
        p.text_paint(&zoom, Vec2::new(150.0, 140.0), &Paint::Gradient(&g));
    }
    let (gpu, cpu) = draw(
        &mut r,
        &mut frames,
        &mut pages,
        &list,
        320,
        200,
        Color::BLACK,
    );
    close(&gpu, &cpu, 3, 0.01);
    compare(&mut r, &mut frames, &list, 320, 200);
    assert_image!("ui_text_paths", &gpu);
    finish(&mut frames, &r);
}

/// Every path tile the GPU builds, against the CPU's walk: the same backdrops, and the same
/// entries (in any order). The walk compares products, not
/// quotients, so the two decide every corner alike.
#[test]
fn path_tiles_match_the_reference() {
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let mut list = UiDrawList::new(1.0);
    {
        let mut p = list.painter(&r.glyph_cache().ids);
        scene(&mut p);
        // Many long spikes, past the surface's left and top, and a star wound in a group.
        let mut spikes = Path::new();
        for k in 0..600 {
            let a = k as f32 * core::f32::consts::TAU / 600.0;
            let radius = if k % 2 == 0 { 230.0 } else { 40.0 };
            let q = Vec2::new(90.0, 60.0) + Vec2::new(a.cos(), a.sin()) * radius;
            if k == 0 {
                spikes.move_to(q);
            } else {
                spikes.line_to(q);
            }
        }
        spikes.close();
        p.fill_path(
            &spikes,
            FillRule::EvenOdd,
            &Paint::Solid(Color::rgba(40, 120, 220, 120)),
        );
    }
    let fonts: [marlin_font::FontView<'_>; 1] = [crate::support::inter()];
    let (w, h) = (320, 200);
    let data = r.prepare(&list, &fonts, None);
    let (tiles, entries) = frames
        .execute(hardware_device(), |g, _| {
            let out = r.add_passes(g, &data, w, h, Color::BLACK);
            let (t, e) = out.paths.expect("paths with tiles");
            (g.readback(t), g.readback(e))
        })
        .unwrap();
    let tiles = frames.wait_readback(tiles).unwrap();
    let entries = frames.wait_readback(entries).unwrap();
    let word = |b: &[u8], i: usize| u32::from_le_bytes(b[4 * i..4 * i + 4].try_into().unwrap());
    let chunk = data.commands();
    let layout = path::layout(chunk, data.glyph_table(), w, h);
    let (mut checked, mut listed) = (0, 0);
    for rec in &layout.records {
        let p = &chunk.payload[rec[0] as usize..];
        let (gx0, gy0, gx1, gy1) = (rec[1] as i32, rec[2] as i32, rec[3] as i32, rec[4] as i32);
        for y in gy0..gy1 {
            for x in gx0..gx1 {
                let t = (rec[5] as i32 + (y - gy0) * (gx1 - gx0) + (x - gx0)) as usize
                    * path::PATH_TILE_WORDS;
                let cpu = path::tile(p, x, y);
                let backdrop = word(&tiles, t) as i32;
                let (count, first) = (word(&tiles, t + 1), word(&tiles, t + 2));
                let mut gpu: Vec<[f32; 4]> = (first..first + count)
                    .map(|i| {
                        [0, 1, 2, 3].map(|k| f32::from_bits(word(&entries, 4 * i as usize + k)))
                    })
                    .collect();
                let mut want = Vec::new();
                for &(i, edge) in &cpu.segments {
                    path::entries(path::segment(p, i as usize), edge, x, y, &mut want);
                }
                let at = format!("path at {} tile ({x}, {y})", rec[0]);
                assert_eq!((backdrop, gpu.len()), (cpu.backdrop, want.len()), "{at}");
                // The same segments, but where one is cut at the tile's side the GPU divides
                // by its own rounding.
                for w in &want {
                    let k = gpu
                        .iter()
                        .position(|g| (0..4).all(|j| (g[j] - w[j]).abs() <= 1e-3))
                        .unwrap_or_else(|| panic!("{at}: {w:?} not in {gpu:?}"));
                    gpu.swap_remove(k);
                }
                checked += 1;
                listed += want.len();
            }
        }
    }
    assert!(
        checked > 100 && listed > 1000,
        "{checked} tiles, {listed} entries"
    );
    finish(&mut frames, &r);
}
