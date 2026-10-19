//! Recording: chunks recorded in parallel keep their order, retained chunks replay as if
//! recorded in place and survive the glyph cache repacking, and text lands on the rasterized
//! glyphs.

use marlin_font::{FontId, GlyphCache, GlyphCacheConfig, GlyphIds, TextLayout, rasterize};
use marlin_math::Vec2;
use marlin_ui_render::command::Kind;
use marlin_ui_render::reference;
use marlin_ui_render::{Color, Rect, RectStyle, RetainedChunk, UiDrawList};

use crate::support::{CpuTexture, CpuTextures, Pages, inter, prepared, scene};

fn panel(p: &mut marlin_ui_render::Painter<'_>, i: usize) {
    let y = i as f32 * 12.0;
    p.rect(
        Rect::new(0.0, y, 40.0, y + 10.0),
        &RectStyle::fill(Color::rgb(i as u8, 0, 0)),
    );
    p.circle(
        Vec2::new(50.0, y + 5.0),
        4.0,
        &marlin_ui_render::ShapeStyle::fill(Color::rgb(0, i as u8, 0)),
    );
}

#[test]
fn chunks_recorded_in_parallel_keep_their_order() {
    let ids = GlyphIds::new();
    let mut serial = UiDrawList::new(1.0);
    {
        let mut p = serial.painter(&ids);
        for i in 0..8 {
            panel(&mut p, i);
        }
    }
    let mut list = UiDrawList::new(1.0);
    let mut slots = Vec::new();
    {
        let mut p = list.painter(&ids);
        panel(&mut p, 0);
        for _ in 1..7 {
            slots.push(p.chunk());
        }
        panel(&mut p, 7);
    }
    // Filled on threads, in reverse.
    std::thread::scope(|s| {
        for (k, slot) in slots.iter_mut().enumerate().rev() {
            let ids = &ids;
            s.spawn(move || panel(&mut slot.painter(ids), k + 1));
        }
    });
    for slot in slots.into_iter().rev() {
        list.fill(slot);
    }
    assert_eq!(list.len(), serial.len());
    assert_eq!(resolved(&list.flatten()), resolved(&serial.flatten()));
}

#[test]
fn retained_chunks_replay_at_an_offset() {
    let ids = GlyphIds::new();
    let font = inter();
    let draw = |p: &mut marlin_ui_render::Painter<'_>, at: Vec2| {
        let layout = TextLayout::new(&font, FontId(0), 13.0, "Retained");
        p.rect(
            Rect::from_min_size(at, Vec2::new(30.0, 8.0)),
            &RectStyle::rounded(Color::WHITE, 2.0),
        );
        p.line(
            at,
            at + Vec2::new(20.0, 9.0),
            &marlin_ui_render::StrokeStyle::new(1.0, Color::BLACK),
        );
        p.triangle(
            [at, at + Vec2::new(5.0, 0.0), at + Vec2::new(0.0, 5.0)],
            &marlin_ui_render::ShapeStyle::fill(Color::BLACK),
        );
        p.circle(
            at + Vec2::new(3.0, 3.0),
            2.0,
            &marlin_ui_render::ShapeStyle::fill(Color::BLACK),
        );
        p.text(&layout, at + Vec2::new(0.25, 10.0), Color::WHITE);
    };
    // At scale 2, a 0.5 logical pixel offset is one physical pixel.
    let retained = RetainedChunk::record(2.0, &ids, |p| draw(p, Vec2::new(3.0, 4.0)));
    let mut replayed = UiDrawList::new(2.0);
    replayed
        .painter(&ids)
        .replay(&retained, Vec2::new(10.5, -1.0));
    let mut direct = UiDrawList::new(2.0);
    draw(&mut direct.painter(&ids), Vec2::new(13.5, 3.0));
    assert_eq!(resolved(&replayed.flatten()), resolved(&direct.flatten()));
    assert_eq!(retained.len(), 4 + "Retained".len());
}

#[test]
fn retained_text_survives_the_glyph_cache_repacking() {
    let font = inter();
    // Small pages, so the second frame's glyphs evict the first frame's page.
    let mut cache = GlyphCache::new(GlyphCacheConfig {
        page_size: 64,
        max_pages: 1,
        gutter: 1,
    });
    let retained = RetainedChunk::record(1.0, &cache.ids, |p| {
        let layout = TextLayout::new(&font, FontId(0), 20.0, "Keep me");
        p.text(&layout, Vec2::new(2.0, 2.0), Color::WHITE);
    });
    let image = |cache: &mut GlyphCache, pages: &mut Pages| {
        let mut list = UiDrawList::new(1.0);
        list.painter(&cache.ids).replay(&retained, Vec2::ZERO);
        let chunk = list.flatten();
        prepared(cache, &chunk, pages);
        let texture = CpuTexture::checker();
        let textures = CpuTextures {
            pages,
            texture: &texture,
        };
        reference::rasterize(&chunk, &cache.table(), &textures, 100, 30, [0.0; 4])
    };
    let mut pages = Pages::new(64);
    let first = image(&mut cache, &mut pages);
    let generation = cache.generation();
    // Another frame needs the only page for other glyphs.
    let mut other = UiDrawList::new(1.0);
    // A glyph too large for the room left: the page is evicted for it.
    let layout = TextLayout::new(&font, FontId(0), 60.0, "W");
    other
        .painter(&cache.ids)
        .text(&layout, Vec2::ZERO, Color::WHITE);
    prepared(&mut cache, &other.flatten(), &mut pages);
    assert!(cache.generation() > generation, "the page was evicted");
    // The retained chunk's commands are unchanged and draw the same.
    assert_eq!(image(&mut cache, &mut pages), first);
}

#[test]
fn text_lands_on_the_rasterized_glyphs() {
    let font = inter();
    let mut cache = GlyphCache::new(GlyphCacheConfig::default());
    let mut list = UiDrawList::new(1.5);
    let layout = TextLayout::new(&font, FontId(0), 12.0, "Ag");
    list.painter(&cache.ids)
        .text(&layout, Vec2::new(1.1, 0.0), Color::WHITE);
    let chunk = list.flatten();
    let mut pages = Pages::new(2048);
    prepared(&mut cache, &chunk, &mut pages);
    let texture = CpuTexture::checker();
    let textures = CpuTextures {
        pages: &pages,
        texture: &texture,
    };
    let image = reference::rasterize(&chunk, &cache.table(), &textures, 40, 30, [0.0; 4]);
    // The 'A': its bitmap at 18 px (12 × 1.5), at the subpixel position of 1.1 × 1.5 = 1.65 px:
    // pen 1, three quarters.
    let c = &chunk.commands[0];
    assert_eq!(Kind::from_word(c[0]), Some(Kind::Glyph));
    let p = &chunk.payload[c[3] as usize..];
    assert_eq!(p[0] as i32, 1);
    let b = rasterize(&font, font.glyph_of('A').unwrap(), 18.0, 3);
    let baseline = (layout.ascent * 1.5).round() as i32;
    assert_eq!(p[1] as i32, baseline);
    for y in 0..b.height as i32 {
        for x in 0..b.width as i32 {
            let (px, py) = (1 + b.left + x, baseline + b.top + y);
            let alpha = image[((py * 40 + px) * 4 + 3) as usize];
            // The 'g' does not reach into the 'A'.
            assert_eq!(
                alpha,
                b.coverage[(y * b.width as i32 + x) as usize],
                "({px}, {py})"
            );
        }
    }
}

#[test]
fn the_scene_records_every_kind() {
    let ids = GlyphIds::new();
    let mut list = UiDrawList::new(1.0);
    scene(&mut list.painter(&ids), 300.0, 190.0, 0);
    let chunk = list.flatten();
    for kind in [
        Kind::Rect,
        Kind::Circle,
        Kind::Line,
        Kind::Triangle,
        Kind::Glyph,
        Kind::Image,
    ] {
        assert!(
            chunk
                .commands
                .iter()
                .any(|c| Kind::from_word(c[0]) == Some(kind)),
            "{kind:?}"
        );
    }
    // Consecutive commands of one color share a paint.
    assert!(chunk.paints.len() < chunk.commands.len());
}

/// A command: kind and flags, paint, payload, group record, clip chain.
pub type Resolved = (u32, [u32; 4], Vec<u32>, Vec<u32>, Vec<u32>);

/// Every command with what it refers to resolved: header, paint, payload, group and clip
/// records. Two lists that draw the same give the same, however their buffers are laid out.
pub fn resolved(c: &marlin_ui_render::command::Chunk) -> Vec<Resolved> {
    use marlin_ui_render::command::{clip_of, group_of};
    c.commands
        .iter()
        .map(|cmd| {
            let kind = Kind::from_word(cmd[0]).unwrap();
            let at = cmd[3] as usize;
            let payload = c.payload[at..at + kind.payload_words() as usize].to_vec();
            // Without the shadow's member range (words 15, 16): positions, not content.
            let mut group = c.groups[group_of(cmd) as usize].to_vec();
            group[15] = 0;
            group[16] = 0;
            // A clip chain, its records without their parent links.
            let mut clip = Vec::new();
            let mut k = clip_of(cmd);
            while k != 0 {
                let r = c.clips[k as usize];
                clip.extend_from_slice(&r[..8]);
                let mut g = c.groups[r[8] as usize];
                g[15] = 0;
                g[16] = 0;
                clip.extend_from_slice(&g);
                clip.extend_from_slice(&r[10..]);
                k = r[9];
            }
            (
                cmd[0] & 0xFFFF,
                c.paints[cmd[2] as usize],
                payload,
                group,
                clip,
            )
        })
        .collect()
}
