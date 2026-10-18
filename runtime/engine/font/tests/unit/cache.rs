//! The glyph cache: stable ids, residency, page LRU, the GPU table and parallel interning.

use marlin_font::{FontId, GlyphCache, GlyphCacheConfig, GlyphId, GlyphKey, rasterize};
use marlin_jobs::{Config, JobSystem};

use crate::inter;

fn ids_for(cache: &GlyphCache, text: &str, size: f32) -> Vec<GlyphId> {
    let font = inter();
    text.chars()
        .map(|c| {
            cache
                .ids
                .id(GlyphKey::new(FontId(0), font.glyph_or_missing(c), size), 0)
        })
        .collect()
}

#[test]
fn ids_are_stable_and_carry_the_subpixel_position() {
    let cache = GlyphCache::new(GlyphCacheConfig::default());
    let key = GlyphKey::new(FontId(0), 36, 13.0);
    let a = cache.ids.id(key, 0);
    let b = cache.ids.id(key, 3);
    assert_eq!(b.0, a.0 + 3);
    assert_eq!(b.subpixel(), 3);
    assert_eq!(cache.ids.id(key, 0), a);
    assert_ne!(cache.ids.id(GlyphKey::new(FontId(0), 36, 13.5), 0), a);
    // Sizes are kept to 1/64 px.
    assert_eq!(cache.ids.id(GlyphKey::new(FontId(0), 36, 13.001), 0), a);
    assert_eq!(cache.ids.key(b), Some((key, 3)));
    assert_eq!(cache.ids.key(GlyphId(1000)), None);
}

#[test]
fn prepared_glyphs_are_resident_and_uploaded_once() {
    let font = inter();
    let mut cache = GlyphCache::new(GlyphCacheConfig::default());
    let ids = ids_for(&cache, "Hello, world", 16.0);
    let stats = cache.prepare(&[font], &ids, None);
    // "Hello, world": H e l o , space w r d — 9 distinct glyphs.
    assert_eq!(stats.rasterized, 9);
    let uploads = cache.take_uploads();
    assert_eq!(uploads.len(), 8, "the space has nothing to upload");
    for u in &uploads {
        assert_eq!(u.coverage.len() as u32, u.rect.width * u.rect.height);
    }
    // Uploads hold what the rasterizer makes, where the table says.
    let table = cache.table();
    let h = ids[0];
    let p = cache.placement(h).unwrap();
    let b = rasterize(&font, font.glyph_of('H').unwrap(), 16.0, 0);
    let up = uploads
        .iter()
        .find(|u| u.rect.x == p.x && u.rect.y == p.y)
        .unwrap();
    assert_eq!(up.coverage, b.coverage);
    assert_eq!(
        table[h.0 as usize],
        [
            p.x | p.y << 16,
            b.width | b.height << 16,
            (b.left as u16 as u32) | (b.top as u16 as u32) << 16,
            0
        ]
    );
    // Again: nothing new.
    let again = cache.prepare(&[font], &ids, None);
    assert_eq!(again.rasterized, 0);
    assert!(cache.take_uploads().is_empty());
}

#[test]
fn full_pages_go_least_recently_used_first() {
    let font = inter();
    // 34² pages hold one 40 px capital each (about 28 × 29 px plus the gutter), two pages.
    let mut cache = GlyphCache::new(GlyphCacheConfig {
        page_size: 34,
        max_pages: 2,
        gutter: 1,
    });
    let [a, b, c] = [0, 1, 2].map(|i| ids_for(&cache, &"ABC"[i..i + 1], 40.0)[0]);
    let page_of = |cache: &GlyphCache, id| cache.placement(id).map(|p| p.page);
    cache.prepare(&[font], &[a], None);
    cache.prepare(&[font], &[b], None);
    assert_eq!((page_of(&cache, a), page_of(&cache, b)), (Some(0), Some(1)));
    let generation = cache.generation();

    // Frame 3 draws B and C: A's page, last used in frame 1, goes.
    let stats = cache.prepare(&[font], &[b, c], None);
    assert_eq!(stats.evicted_pages, 1);
    assert!(cache.generation() > generation);
    assert_eq!(page_of(&cache, a), None);
    assert_eq!((page_of(&cache, b), page_of(&cache, c)), (Some(1), Some(0)));

    // A frame drawing all three: nothing it uses is evicted, so A finds no room and draws
    // nothing, with no size in the table.
    let stats = cache.prepare(&[font], &[a, b, c], None);
    assert_eq!((stats.evicted_pages, stats.dropped), (0, 1));
    assert_eq!(page_of(&cache, a), None);
    assert_eq!(cache.table()[a.0 as usize][1], 0);
    // The next frame without B: A comes back in B's page.
    let stats = cache.prepare(&[font], &[a, c], None);
    assert_eq!((stats.rasterized, stats.evicted_pages), (1, 1));
    assert_eq!(page_of(&cache, a), Some(1));
}

#[test]
fn ids_intern_in_parallel_and_glyphs_rasterize_in_jobs() {
    let font = inter();
    let mut cache = GlyphCache::new(GlyphCacheConfig::default());
    let text = "The quick brown fox jumps over the lazy dog 0123456789";
    std::thread::scope(|s| {
        for t in 0..4 {
            let cache = &cache;
            s.spawn(move || ids_for(cache, &text[t..], 18.0));
        }
    });
    let ids = ids_for(&cache, text, 18.0);
    let distinct: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(
        cache.ids.len() as usize,
        4 * distinct.len(),
        "every key once"
    );
    let jobs = JobSystem::new(Config::default()).unwrap();
    let stats = cache.prepare(&[font], &ids, Some(&jobs));
    assert_eq!(stats.rasterized as usize, distinct.len());
    let mut serial = GlyphCache::new(GlyphCacheConfig::default());
    let serial_ids = ids_for(&serial, text, 18.0);
    serial.prepare(&[font], &serial_ids, None);
    // The parallel interning numbered the glyphs in another order, so compare bitmaps as sets.
    let bitmaps = |c: &mut GlyphCache| {
        let mut v: Vec<_> = c.take_uploads().into_iter().map(|u| u.coverage).collect();
        v.sort();
        v
    };
    assert_eq!(
        bitmaps(&mut cache),
        bitmaps(&mut serial),
        "jobs change nothing"
    );
}
