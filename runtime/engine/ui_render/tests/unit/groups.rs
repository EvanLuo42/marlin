//! Groups and clips (§2.2, §3.2): coverage under transforms against supersampled shapes, clip
//! chains, nesting, and chunks and replays keeping the state they were recorded in.

use marlin_font::GlyphIds;
use marlin_math::Vec2;
use marlin_ui_render::command::{Chunk, clip_of, group_of};
use marlin_ui_render::reference::{bin, tile_counts};
use marlin_ui_render::shape::{Scene, Textures, shade};
use marlin_ui_render::{
    Affine2, Color, Corners, GroupDesc, Painter, Rect, RectStyle, RetainedChunk, ShapeStyle,
    StrokeStyle, UiDrawList,
};

use crate::recording::resolved;

struct NoTextures;

impl Textures for NoTextures {
    fn glyph(&self, _: u32, _: u32, _: u32) -> f32 {
        0.0
    }
    fn sample(&self, _: u32, _: u32, _: [f32; 2], _: f32) -> [f32; 4] {
        [0.0; 4]
    }
}

fn record(scale: f32, f: impl FnOnce(&mut Painter<'_>)) -> Chunk {
    let ids = GlyphIds::new();
    let mut list = UiDrawList::new(scale);
    f(&mut list.painter(&ids));
    list.flatten()
}

/// Alpha of every command blended, against `inside` supersampled 16 × 16 per pixel over 64 × 64
/// pixels: worst and mean difference.
fn compare(chunk: &Chunk, inside: impl Fn(f32, f32) -> bool) -> (f32, f32) {
    let n = 16;
    let scene = Scene::new(chunk, &[]);
    let (mut worst, mut sum) = (0.0f32, 0.0);
    for py in 0..64 {
        for px in 0..64 {
            let mut a = 0.0;
            for c in &chunk.commands {
                let s = shade(c, &scene, &NoTextures, px as f32 + 0.5, py as f32 + 0.5)[3];
                a = s + a * (1.0 - s);
            }
            let mut k = 0;
            for sy in 0..n {
                for sx in 0..n {
                    let x = px as f32 + (sx as f32 + 0.5) / n as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / n as f32;
                    k += u32::from(inside(x, y));
                }
            }
            let exact = k as f32 / (n * n) as f32;
            worst = worst.max((a - exact).abs());
            sum += (a - exact).abs();
        }
    }
    (worst, sum / (64.0 * 64.0))
}

fn in_rect(r: Rect, p: Vec2) -> bool {
    p.x >= r.min.x && p.x < r.max.x && p.y >= r.min.y && p.y < r.max.y
}

#[test]
fn rotated_and_scaled_groups_keep_one_pixel_edges() {
    // A 20 × 10 rectangle, scaled 2× and rotated 30° about the surface's middle.
    let r = Rect::new(-10.0, -5.0, 10.0, 5.0);
    let t = Affine2::translate(32.0, 32.0) * Affine2::rotate(0.5236) * Affine2::scale(2.0, 2.0);
    let chunk = record(1.0, |p| {
        p.begin_group(&GroupDesc {
            transform: t,
            ..GroupDesc::default()
        });
        p.rect(r, &RectStyle::fill(Color::WHITE));
        p.end_group();
    });
    let inv = t.inverse();
    let (worst, mean) = compare(&chunk, |x, y| in_rect(r, inv.apply(Vec2::new(x, y))));
    assert!(worst < 0.15 && mean < 0.01, "similarity: {worst} {mean}");

    // Non-uniform: 3× across, ½× down, rotated; the coverage follows the distance's gradient.
    let t = Affine2::translate(32.0, 30.0) * Affine2::rotate(0.3) * Affine2::scale(3.0, 0.5);
    let chunk = record(1.0, |p| {
        p.begin_group(&GroupDesc {
            transform: t,
            ..GroupDesc::default()
        });
        p.rect(
            Rect::new(-8.0, -30.0, 8.0, 30.0),
            &RectStyle::fill(Color::WHITE),
        );
        p.end_group();
    });
    let inv = t.inverse();
    let (worst, mean) = compare(&chunk, |x, y| {
        in_rect(
            Rect::new(-8.0, -30.0, 8.0, 30.0),
            inv.apply(Vec2::new(x, y)),
        )
    });
    assert!(worst < 0.25 && mean < 0.01, "non-uniform: {worst} {mean}");
}

#[test]
fn nested_groups_multiply_transforms_and_opacity() {
    let chunk = record(2.0, |p| {
        p.begin_group(&GroupDesc {
            transform: Affine2::translate(10.0, 4.0),
            opacity: 0.5,
            ..GroupDesc::default()
        });
        p.begin_group(&GroupDesc {
            transform: Affine2::scale(2.0, 2.0),
            opacity: 0.5,
            ..GroupDesc::default()
        });
        p.circle(Vec2::new(3.0, 3.0), 2.0, &ShapeStyle::fill(Color::WHITE));
        p.end_group();
        p.end_group();
    });
    let c = &chunk.commands[0];
    let g = &chunk.groups[group_of(c) as usize];
    let m: Vec<f32> = g[..6].iter().map(|&w| f32::from_bits(w)).collect();
    // Surface scale 2, then translate (10, 4), then scale 2.
    assert_eq!(m, [4.0, 0.0, 0.0, 4.0, 20.0, 8.0]);
    assert_eq!(f32::from_bits(g[12]), 0.25);
    // The circle's center (3, 3) lands at (32, 20) on the screen, at a quarter of its alpha.
    let scene = Scene::new(&chunk, &[]);
    let a = shade(c, &scene, &NoTextures, 32.5, 20.5)[3];
    assert!((a - 0.25).abs() < 1e-6, "{a}");
}

#[test]
fn clips_cut_and_chains_intersect() {
    let outer = Rect::new(8.0, 8.0, 56.0, 40.0);
    let inner = Rect::new(20.0, 0.0, 70.0, 70.0);
    let chunk = record(1.0, |p| {
        p.push_clip(outer, Corners::all(10.0));
        p.push_clip(inner, Corners::ZERO);
        p.rect(
            Rect::new(0.0, 0.0, 64.0, 64.0),
            &RectStyle::fill(Color::WHITE),
        );
        p.pop_clip();
        p.pop_clip();
        // Outside every clip again.
        p.line(
            Vec2::new(0.0, 60.0),
            Vec2::new(64.0, 60.0),
            &StrokeStyle::new(2.0, Color::WHITE),
        );
    });
    assert_eq!(clip_of(&chunk.commands[1]), 0);
    let rounded = |x: f32, y: f32| {
        let cx = x.clamp(outer.min.x + 10.0, outer.max.x - 10.0);
        let cy = y.clamp(outer.min.y + 10.0, outer.max.y - 10.0);
        in_rect(outer, Vec2::new(x, y)) && (x - cx).hypot(y - cy) <= 10.0
    };
    let (worst, mean) = compare(&chunk, |x, y| {
        (rounded(x, y) && in_rect(inner, Vec2::new(x, y))) || (59.0..61.0).contains(&y)
    });
    assert!(worst < 0.15 && mean < 0.01, "{worst} {mean}");
    // The clip's box bounds what it holds: no tile outside it lists the rectangle.
    let tiles = bin(&chunk, &[], 64, 64);
    let (tx, _) = tile_counts(64, 64);
    for (i, t) in tiles.iter().enumerate() {
        let (x, y) = (i as u32 % tx, i as u32 / tx);
        if x == 0 || y >= 3 {
            assert!(!t.commands.contains(&0), "tile ({x}, {y})");
        }
    }
}

#[test]
fn a_rotated_clip_cuts_at_an_angle() {
    let t = Affine2::translate(32.0, 32.0) * Affine2::rotate(0.7);
    let clip = Rect::new(-16.0, -10.0, 16.0, 10.0);
    let chunk = record(1.0, |p| {
        p.begin_group(&GroupDesc {
            transform: t,
            ..GroupDesc::default()
        });
        p.push_clip(clip, Corners::ZERO);
        p.end_group();
        // The clip outlives the group that placed it.
        p.rect(
            Rect::new(0.0, 0.0, 64.0, 64.0),
            &RectStyle::fill(Color::WHITE),
        );
        p.pop_clip();
    });
    let inv = t.inverse();
    let (worst, mean) = compare(&chunk, |x, y| in_rect(clip, inv.apply(Vec2::new(x, y))));
    assert!(worst < 0.15 && mean < 0.01, "{worst} {mean}");
    // A rotated clip covers no screen rectangle for sure: nothing opaque inside it.
    let tiles = bin(&chunk, &[], 64, 64);
    assert!(tiles.iter().all(|t| t.commands.len() <= 1));
}

#[test]
fn opaque_interiors_follow_groups_and_clips() {
    let chunk = record(1.0, |p| {
        p.rect(
            Rect::new(0.0, 0.0, 128.0, 64.0),
            &RectStyle::fill(Color::BLACK),
        );
        // Scaled 2×: covers tiles (2..6, 0..2) of the surface.
        p.begin_group(&GroupDesc {
            transform: Affine2::scale(2.0, 2.0),
            ..GroupDesc::default()
        });
        p.rect(
            Rect::new(16.0, 0.0, 48.0, 16.0),
            &RectStyle::fill(Color::WHITE),
        );
        p.end_group();
        // Half transparent: hides nothing.
        p.begin_group(&GroupDesc {
            opacity: 0.5,
            ..GroupDesc::default()
        });
        p.rect(
            Rect::new(0.0, 32.0, 128.0, 64.0),
            &RectStyle::fill(Color::WHITE),
        );
        p.end_group();
        // Clipped to its left half: hides only there.
        p.push_clip(Rect::new(0.0, 0.0, 32.0, 64.0), Corners::ZERO);
        p.rect(
            Rect::new(0.0, 0.0, 128.0, 64.0),
            &RectStyle::fill(Color::rgb(1, 2, 3)),
        );
        p.pop_clip();
    });
    let tiles = bin(&chunk, &[], 128, 64);
    let (tx, _) = tile_counts(128, 64);
    let at = |x: u32, y: u32| &tiles[(y * tx + x) as usize].commands;
    assert_eq!(at(2, 0), &[1]);
    // The translucent rectangle's anti-aliased edge reaches into row 1.
    assert_eq!(at(5, 1), &[1, 2]);
    assert_eq!(at(6, 0), &[0, 1], "the scaled rectangle's edge at x = 96");
    assert_eq!(at(7, 0), &[0]);
    assert_eq!(at(3, 3), &[0, 2]);
    assert_eq!(at(1, 3), &[3]);
    assert_eq!(at(2, 3), &[0, 2]);
}

#[test]
fn slots_and_replays_keep_the_group_and_clip_they_were_made_in() {
    let ids = GlyphIds::new();
    let draw = |p: &mut Painter<'_>| {
        p.rect(
            Rect::new(1.0, 2.0, 30.0, 20.0),
            &RectStyle::rounded(Color::WHITE, 3.0),
        );
        p.circle(Vec2::new(5.0, 5.0), 4.0, &ShapeStyle::fill(Color::BLACK));
    };
    let enter = |p: &mut Painter<'_>| {
        p.push_clip(Rect::new(0.0, 0.0, 50.0, 50.0), Corners::all(4.0));
        p.begin_group(&GroupDesc {
            transform: Affine2::translate(3.0, 7.0) * Affine2::rotate(0.25),
            opacity: 0.75,
            ..GroupDesc::default()
        });
        p.push_clip(Rect::new(0.0, 0.0, 20.0, 20.0), Corners::ZERO);
    };
    let leave = |p: &mut Painter<'_>| {
        p.pop_clip();
        p.end_group();
        p.pop_clip();
    };
    let mut direct = UiDrawList::new(1.5);
    {
        let mut p = direct.painter(&ids);
        enter(&mut p);
        draw(&mut p);
        leave(&mut p);
    }
    let mut slotted = UiDrawList::new(1.5);
    let mut slot = {
        let mut p = slotted.painter(&ids);
        enter(&mut p);
        let slot = p.chunk();
        leave(&mut p);
        slot
    };
    draw(&mut slot.painter(&ids));
    slotted.fill(slot);
    assert_eq!(resolved(&slotted.flatten()), resolved(&direct.flatten()));

    // A replay takes the clip it is replayed under.
    let retained = RetainedChunk::record(1.0, &ids, draw);
    let mut replayed = UiDrawList::new(1.0);
    {
        let mut p = replayed.painter(&ids);
        p.push_clip(Rect::new(4.0, 4.0, 12.0, 12.0), Corners::ZERO);
        p.replay(&retained, Vec2::new(2.0, 3.0));
        p.pop_clip();
    }
    let mut expected = UiDrawList::new(1.0);
    {
        let mut p = expected.painter(&ids);
        p.push_clip(Rect::new(4.0, 4.0, 12.0, 12.0), Corners::ZERO);
        p.rect(
            Rect::new(3.0, 5.0, 32.0, 23.0),
            &RectStyle::rounded(Color::WHITE, 3.0),
        );
        p.circle(Vec2::new(7.0, 8.0), 4.0, &ShapeStyle::fill(Color::BLACK));
        p.pop_clip();
    }
    assert_eq!(resolved(&replayed.flatten()), resolved(&expected.flatten()));
}
