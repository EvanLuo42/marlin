//! Text drawn from its outlines (§6.4): large text matches the rasterized glyph, and text goes
//! to paths when large, turned, skewed, stretched or zooming, and only when the painter has the
//! fonts.

use marlin_font::{FontId, GlyphIds, TextLayout, rasterize};
use marlin_math::Vec2;
use marlin_ui_render::command::{Chunk, Kind};
use marlin_ui_render::shape::{Scene, Textures, shade};
use marlin_ui_render::{Affine2, Color, GroupDesc, Painter, UiDrawList};

use crate::support::inter;

struct NoTextures;

impl Textures for NoTextures {
    fn glyph(&self, _: u32, _: u32, _: u32) -> f32 {
        0.0
    }
    fn sample(&self, _: u32, _: u32, _: [f32; 2], _: f32) -> [f32; 4] {
        [0.0; 4]
    }
}

fn record(with_fonts: bool, f: impl FnOnce(&mut Painter<'_>)) -> Chunk {
    let ids = GlyphIds::new();
    let fonts = [inter()];
    let mut list = UiDrawList::new(1.0);
    let painter = list.painter(&ids);
    f(&mut if with_fonts {
        painter.with_fonts(&fonts)
    } else {
        painter
    });
    list.flatten()
}

fn kinds(c: &Chunk) -> Vec<Kind> {
    c.commands
        .iter()
        .map(|c| Kind::from_word(c[0]).unwrap())
        .collect()
}

#[test]
fn large_text_as_a_path_covers_what_the_rasterizer_does() {
    let font = inter();
    let size = 80.0;
    let layout = TextLayout::new(&font, FontId(0), size, "&");
    // The pen on whole pixels, the baseline too: the rasterizer's subpixel 0.
    let origin = Vec2::new(10.0, 4.0 + layout.ascent.round() - layout.ascent);
    let chunk = record(true, |p| p.text(&layout, origin, Color::WHITE));
    assert_eq!(kinds(&chunk), [Kind::Path]);
    let bitmap = rasterize(&font, font.glyph_of('&').unwrap(), size, 0);
    let pen = Vec2::new(origin.x, origin.y + layout.ascent);
    let scene = Scene::new(&chunk, &[]);
    let (mut worst, mut sum) = (0.0f32, 0.0);
    for y in 0..bitmap.height as i32 {
        for x in 0..bitmap.width as i32 {
            let (px, py) = (
                pen.x + (bitmap.left + x) as f32,
                pen.y + (bitmap.top + y) as f32,
            );
            let a = shade(&chunk.commands[0], &scene, &NoTextures, px + 0.5, py + 0.5)[3];
            let b = bitmap.coverage[(y * bitmap.width as i32 + x) as usize] as f32 / 255.0;
            worst = worst.max((a - b).abs());
            sum += (a - b).abs();
        }
    }
    let mean = sum / (bitmap.width * bitmap.height) as f32;
    // Both are exact areas of the same curves, flattened differently (0.05 px against 1/64 px);
    // where the glyph's contours overlap, a path pixel counts the overlap twice (§4.2).
    assert!(worst < 0.1 && mean < 0.004, "{worst} {mean}");
}

#[test]
fn which_text_goes_to_paths() {
    let font = inter();
    let small = TextLayout::new(&font, FontId(0), 14.0, "ab");
    let large = TextLayout::new(&font, FontId(0), 72.0, "ab");
    // Small and upright: the glyph cache. Large: paths. Without the fonts: always the cache.
    assert_eq!(
        kinds(&record(true, |p| p.text(&small, Vec2::ZERO, Color::WHITE))),
        [Kind::Glyph; 2]
    );
    assert_eq!(
        kinds(&record(true, |p| p.text(&large, Vec2::ZERO, Color::WHITE))),
        [Kind::Path; 2]
    );
    assert_eq!(
        kinds(&record(false, |p| p.text(&large, Vec2::ZERO, Color::WHITE))),
        [Kind::Glyph; 2]
    );
    // Small text zooming, turned, or stretched: paths; scaled uniformly past 64 px: paths.
    let zooming = record(true, |p| {
        p.set_text_as_paths(true);
        p.text(&small, Vec2::ZERO, Color::WHITE);
    });
    assert_eq!(kinds(&zooming), [Kind::Path; 2]);
    for (t, expected) in [
        (Affine2::rotate(0.3), Kind::Path),
        (Affine2::scale(2.0, 1.0), Kind::Path),
        (Affine2::scale(2.0, 2.0), Kind::Glyph),
        (Affine2::scale(5.0, 5.0), Kind::Path),
    ] {
        let chunk = record(true, |p| {
            p.begin_group(&GroupDesc {
                transform: t,
                ..GroupDesc::default()
            });
            p.text(&small, Vec2::ZERO, Color::WHITE);
            p.end_group();
        });
        assert_eq!(kinds(&chunk), [expected; 2], "{t:?}");
    }
}

#[test]
fn turned_text_turns() {
    // A tall "l" turned a quarter: its path's box is wide and short.
    let font = inter();
    let layout = TextLayout::new(&font, FontId(0), 40.0, "l");
    let chunk = record(true, |p| {
        p.begin_group(&GroupDesc {
            transform: Affine2::translate(60.0, 20.0) * Affine2([0.0, 1.0, -1.0, 0.0, 0.0, 0.0]),
            ..GroupDesc::default()
        });
        p.text(&layout, Vec2::ZERO, Color::WHITE);
        p.end_group();
    });
    let p = &chunk.payload[chunk.commands[0][3] as usize..];
    let w = |i: usize| f32::from_bits(p[i]);
    let (width, height) = (w(7) - w(5), w(8) - w(6));
    assert!(width > 3.0 * height, "{width} × {height}");
}
