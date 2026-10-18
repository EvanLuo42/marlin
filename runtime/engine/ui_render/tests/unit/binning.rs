//! The reference binning: every command is listed wherever it has ink, lines only near their
//! path, opaque panels drop what they hide, and none of that changes the image.

use marlin_font::{GlyphCache, GlyphCacheConfig};
use marlin_math::Vec2;
use marlin_ui_render::command::{Chunk, OPAQUE, TILE};
use marlin_ui_render::reference::{RefTile, bin, rasterize, tile_counts};
use marlin_ui_render::shape::{over, shade};
use marlin_ui_render::{Color, Rect, RectStyle, StrokeStyle, UiDrawList};

use crate::support::{CpuTexture, CpuTextures, Pages, TEXTURE_VIEW, prepared, scene};

const W: u32 = 300;
const H: u32 = 190;

fn scene_chunk(cache: &GlyphCache) -> Chunk {
    let mut list = UiDrawList::new(1.0);
    scene(
        &mut list.painter(&cache.ids),
        W as f32,
        H as f32,
        TEXTURE_VIEW,
    );
    list.flatten()
}

#[test]
fn commands_are_listed_wherever_they_have_ink() {
    let mut cache = GlyphCache::new(GlyphCacheConfig::default());
    let mut chunk = scene_chunk(&cache);
    // Without the opaque background and panel, nothing is hidden.
    chunk.commands.drain(..2);
    let mut pages = Pages::new(2048);
    prepared(&mut cache, &chunk, &mut pages);
    let glyphs = cache.table();
    let texture = CpuTexture::checker();
    let textures = CpuTextures {
        pages: &pages,
        texture: &texture,
    };
    let tiles = bin(&chunk, &glyphs, W, H);
    let (tx, _) = tile_counts(W, H);
    for y in 0..H {
        for x in 0..W {
            let tile = &tiles[((y / TILE) * tx + x / TILE) as usize];
            for (i, c) in chunk.commands.iter().enumerate() {
                let a = shade(
                    c,
                    &chunk.payload,
                    &chunk.paints,
                    &glyphs,
                    &textures,
                    x as f32 + 0.5,
                    y as f32 + 0.5,
                )[3];
                if a > 0.0 {
                    let at = tile.commands.iter().position(|&k| k == i as u32);
                    assert!(
                        at.is_some_and(|k| !tile.optional[k]),
                        "command {i} has ink at ({x}, {y}) but is not listed for its tile"
                    );
                }
            }
        }
    }
}

#[test]
fn diagonal_lines_skip_the_tiles_they_miss() {
    let ids = marlin_font::GlyphIds::new();
    let mut list = UiDrawList::new(1.0);
    list.painter(&ids).line(
        Vec2::new(0.0, 0.0),
        Vec2::new(W as f32, H as f32),
        &StrokeStyle::new(1.0, Color::WHITE),
    );
    let tiles = bin(&list.flatten(), &[], W, H);
    let listed = tiles.iter().filter(|t| !t.commands.is_empty()).count();
    let (tx, ty) = tile_counts(W, H);
    // The bounding box is every tile; the line passes through about tx + ty of them.
    assert!(
        listed < (tx + ty) as usize * 2,
        "{listed} of {} tiles",
        tx * ty
    );
    assert!(listed >= tx.max(ty) as usize);
}

#[test]
fn opaque_panels_drop_what_they_hide_without_changing_the_image() {
    let mut cache = GlyphCache::new(GlyphCacheConfig::default());
    let chunk = scene_chunk(&cache);
    let mut pages = Pages::new(2048);
    prepared(&mut cache, &chunk, &mut pages);
    let glyphs = cache.table();
    let texture = CpuTexture::checker();
    let textures = CpuTextures {
        pages: &pages,
        texture: &texture,
    };
    let tiles = bin(&chunk, &glyphs, W, H);
    // The background covers everything: every tile starts with it or with an opaque command
    // that hides it, and tiles under the panel's middle start with the panel.
    for t in &tiles {
        let first = t.commands[0];
        assert!(
            first == 0 || chunk.commands[first as usize][0] & OPAQUE != 0,
            "{t:?}"
        );
    }
    let (tx, _) = tile_counts(W, H);
    let inner = &tiles[(3 * tx + 2) as usize];
    assert_eq!(inner.commands[0], 1, "{inner:?}");

    // Every pixel against every command, nothing dropped.
    let image = rasterize(&chunk, &glyphs, &textures, W, H, [0.0, 0.0, 0.0, 1.0]);
    for y in 0..H {
        for x in 0..W {
            let mut color = [0.0, 0.0, 0.0, 1.0];
            for c in &chunk.commands {
                let src = shade(
                    c,
                    &chunk.payload,
                    &chunk.paints,
                    &glyphs,
                    &textures,
                    x as f32 + 0.5,
                    y as f32 + 0.5,
                );
                color = over(src, color);
            }
            let at = ((y * W + x) * 4) as usize;
            for k in 0..4 {
                let v = (color[k].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                assert_eq!(image[at + k], v, "({x}, {y}) channel {k}");
            }
        }
    }
}

#[test]
fn translucent_panels_drop_nothing() {
    let ids = marlin_font::GlyphIds::new();
    let mut list = UiDrawList::new(1.0);
    let mut p = list.painter(&ids);
    p.rect(
        Rect::new(0.0, 0.0, 64.0, 64.0),
        &RectStyle::fill(Color::WHITE),
    );
    p.rect(
        Rect::new(0.0, 0.0, 64.0, 64.0),
        &RectStyle::fill(Color::rgba(0, 0, 0, 254)),
    );
    let tiles = bin(&list.flatten(), &[], 64, 64);
    assert!(tiles.iter().all(|t| t.commands == [0, 1]));
}

#[test]
fn optional_entries_may_be_missing() {
    let t = RefTile {
        commands: vec![1, 2, 3],
        optional: vec![false, true, false],
    };
    assert!(t.matches(&[1, 2, 3]));
    assert!(t.matches(&[1, 3]));
    assert!(!t.matches(&[1]));
    assert!(!t.matches(&[2, 3]));
    assert!(!t.matches(&[1, 3, 2]));
}
