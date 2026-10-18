//! The GPU's tile lists against the CPU reference's, entry by entry.

use marlin_font::FontView;
use marlin_gpu_test_support::hardware_device;
use marlin_math::{Rng, Vec2};
use marlin_render_graph::{FrameContext, FrameContextDesc};
use marlin_ui_render::command::{BLOCK_COMMANDS, BLOCK_WORDS};
use marlin_ui_render::reference::{bin, tile_counts};
use marlin_ui_render::{
    Color, Corners, LineCap, Rect, RectStyle, ShapeStyle, Stroke, StrokeStyle, UiDrawList,
    UiRenderer, UiRendererDesc,
};

use crate::support::{inter, scene};
use crate::{finish, renderer};

/// The tile lists the GPU builds for `list` on a `width` × `height` surface.
fn gpu_lists(
    r: &mut UiRenderer,
    frames: &mut FrameContext,
    list: &UiDrawList,
    width: u32,
    height: u32,
) -> (Vec<Vec<u32>>, marlin_ui_render::UiFrameData) {
    let fonts: [FontView<'_>; 1] = [inter()];
    let data = r.prepare(list, &fonts, None);
    let (heads, pool) = frames
        .execute(hardware_device(), |g, _| {
            let out = r.add_passes(g, &data, width, height, Color::BLACK);
            (g.readback(out.heads), g.readback(out.pool))
        })
        .unwrap();
    let heads = frames.wait_readback(heads).unwrap();
    let pool = frames.wait_readback(pool).unwrap();
    let word = |b: &[u8], i: usize| u32::from_le_bytes(b[4 * i..4 * i + 4].try_into().unwrap());
    let (tx, ty) = tile_counts(width, height);
    let mut lists = Vec::new();
    for t in 0..(tx * ty) as usize {
        let (mut block, count) = (word(&heads, 2 * t), word(&heads, 2 * t + 1));
        let mut l = Vec::with_capacity(count as usize);
        for k in 0..count {
            let slot = k % BLOCK_COMMANDS;
            if slot == 0 && k > 0 {
                block = word(&pool, (block * BLOCK_WORDS + BLOCK_COMMANDS) as usize);
            }
            l.push(word(&pool, (block * BLOCK_WORDS + slot) as usize));
        }
        lists.push(l);
    }
    (lists, data)
}

fn compare(
    r: &mut UiRenderer,
    frames: &mut FrameContext,
    list: &UiDrawList,
    width: u32,
    height: u32,
) {
    let (gpu, data) = gpu_lists(r, frames, list, width, height);
    let reference = bin(data.commands(), data.glyph_table(), width, height);
    let (tx, _) = tile_counts(width, height);
    let mut entries = 0;
    for (t, (g, c)) in gpu.iter().zip(&reference).enumerate() {
        assert!(
            c.matches(g),
            "tile ({}, {}) of {width}×{height}: GPU {g:?}, reference {c:?}",
            t as u32 % tx,
            t as u32 / tx
        );
        entries += g.len();
    }
    assert!(entries > 0);
    assert!(!r.pool_overflowed());
}

#[test]
fn the_scene_bins_like_the_reference() {
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    // Sizes that are not whole tiles or bins, one of them more than a bin across.
    for (w, h) in [(300, 190), (517, 301), (33, 17)] {
        let mut list = UiDrawList::new(1.0);
        scene(
            &mut list.painter(&r.glyph_cache().ids),
            w as f32,
            h as f32,
            0,
        );
        compare(&mut r, &mut frames, &list, w, h);
    }
    finish(&mut frames, &r);
}

/// Many random commands: more than one 256-command chunk, lines in every direction, opaque
/// panels hiding some, and long lists that take several blocks.
#[test]
fn random_commands_bin_like_the_reference() {
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let (w, h) = (700u32, 420u32);
    let mut rng = Rng::new(0x0b1d);
    for round in 0..3 {
        let mut list = UiDrawList::new(1.0);
        let mut p = list.painter(&r.glyph_cache().ids);
        let pt = |rng: &mut Rng| {
            Vec2::new(
                rng.range_f32(-40.0, w as f32 + 40.0),
                rng.range_f32(-40.0, h as f32 + 40.0),
            )
        };
        let color = |rng: &mut Rng| {
            Color::rgba(
                rng.next_u32() as u8,
                rng.next_u32() as u8,
                rng.next_u32() as u8,
                if rng.bool() { 255 } else { 128 },
            )
        };
        for _ in 0..900 + round * 300 {
            match rng.below(4) {
                0 => {
                    let a = pt(&mut rng);
                    let size = Vec2::new(rng.range_f32(1.0, 300.0), rng.range_f32(1.0, 200.0));
                    p.rect(
                        Rect::from_min_size(a, size),
                        &RectStyle {
                            fill: color(&mut rng),
                            corners: Corners::all(rng.range_f32(0.0, 12.0)),
                            stroke: rng.bool().then_some(Stroke {
                                width: 1.5,
                                color: Color::WHITE,
                            }),
                        },
                    );
                }
                1 => p.circle(
                    pt(&mut rng),
                    rng.range_f32(0.5, 90.0),
                    &ShapeStyle::fill(color(&mut rng)),
                ),
                2 => {
                    let cap = [LineCap::Butt, LineCap::Round, LineCap::Square][rng.below(3)];
                    let style = StrokeStyle {
                        width: rng.range_f32(0.5, 9.0),
                        color: color(&mut rng),
                        cap,
                        dash: None,
                    };
                    p.line(pt(&mut rng), pt(&mut rng), &style);
                }
                _ => p.triangle(
                    [pt(&mut rng), pt(&mut rng), pt(&mut rng)],
                    &ShapeStyle::fill(color(&mut rng)),
                ),
            }
        }
        compare(&mut r, &mut frames, &list, w, h);
    }
    finish(&mut frames, &r);
}

#[test]
fn an_empty_list_lists_nothing() {
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let (gpu, _) = gpu_lists(&mut r, &mut frames, &UiDrawList::new(1.0), 64, 40);
    assert!(gpu.iter().all(Vec::is_empty));
    finish(&mut frames, &r);
}

#[test]
fn a_small_pool_overflows_and_grows() {
    let mut r = renderer(&UiRendererDesc {
        initial_pool_blocks: 64,
        ..UiRendererDesc::default()
    });
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    // 40 × 26 tiles, each taking a block for the background: more than 64.
    let mut list = UiDrawList::new(1.0);
    list.painter(&r.glyph_cache().ids).rect(
        Rect::new(0.0, 0.0, 640.0, 416.0),
        &RectStyle::fill(Color::rgba(10, 20, 30, 200)),
    );
    let before = r.pool_blocks();
    // The overflow shows when the frame's slot comes round again.
    for _ in 0..4 {
        let _ = gpu_lists(&mut r, &mut frames, &list, 640, 416);
    }
    assert!(r.pool_overflowed());
    assert!(r.pool_blocks() > before);
    // Grown: the lists are whole again.
    for _ in 0..8 {
        let _ = gpu_lists(&mut r, &mut frames, &list, 640, 416);
    }
    let _ = r.pool_overflowed();
    compare(&mut r, &mut frames, &list, 640, 416);
    finish(&mut frames, &r);
}
