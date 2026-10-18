//! The glyph cache (0012 §6.3): rasterized glyphs packed into pages, found through stable
//! glyph ids.
//!
//! - A glyph is keyed by (font, glyph, pixel size, subpixel position). [`GlyphIds`] gives each
//!   key a [`GlyphId`] once and forever; draw commands store ids only. Ids are interned under a
//!   lock, so panels recording in parallel can share one interner.
//! - [`GlyphCache::prepare`] makes a frame's glyphs resident: the ones not in a page are
//!   rasterized (in jobs when given a job system) and packed into a page with the skyline
//!   packer. The caller uploads the new bitmaps ([`GlyphCache::take_uploads`]) and the id → page
//!   rectangle table ([`GlyphCache::table`]) before drawing.
//! - Pages are evicted whole, least recently used first, never a page the frame uses; its glyphs
//!   are rasterized again the next time they are drawn. Commands and retained chunks stay valid,
//!   since they never name a place in a page.

use alloc::vec;
use alloc::vec::Vec;

use marlin_containers::{HashMap, HashMapExt};
use marlin_jobs::JobSystem;
use marlin_math::scalar::round;
use marlin_sync::Mutex;

use crate::asset::FontView;
use crate::pack::{Rect, Skyline};
use crate::raster::{GlyphBitmap, SUBPIXEL_POSITIONS, rasterize};

/// A font, by its index in the slice of fonts [`GlyphCache::prepare`] is given.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct FontId(pub u32);

/// What a rasterized glyph is, but for its subpixel position.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GlyphKey {
    pub font: FontId,
    pub glyph: u32,
    /// Pixels per em, in 1/64 px.
    pub size: u32,
}

impl GlyphKey {
    /// The key of `glyph` at `size` px per em (rounded to 1/64 px).
    pub fn new(font: FontId, glyph: u32, size: f32) -> Self {
        Self {
            font,
            glyph,
            size: round(size * 64.0).max(1.0) as u32,
        }
    }

    pub fn size_px(&self) -> f32 {
        self.size as f32 / 64.0
    }
}

/// A glyph at one subpixel position: `key index * SUBPIXEL_POSITIONS + subpixel`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct GlyphId(pub u32);

impl GlyphId {
    pub fn subpixel(self) -> u32 {
        self.0 % SUBPIXEL_POSITIONS
    }
}

#[derive(Default)]
struct Interner {
    map: HashMap<GlyphKey, u32>,
    keys: Vec<GlyphKey>,
}

/// The key → id table, shared by everything that records text.
pub struct GlyphIds {
    inner: Mutex<Interner>,
}

impl Default for GlyphIds {
    fn default() -> Self {
        Self::new()
    }
}

impl GlyphIds {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Interner {
                map: HashMap::new(),
                keys: Vec::new(),
            }),
        }
    }

    /// The id of `key` at `subpixel` (below [`SUBPIXEL_POSITIONS`]).
    pub fn id(&self, key: GlyphKey, subpixel: u32) -> GlyphId {
        let mut i = self.inner.lock();
        GlyphId(Self::intern(&mut i, key) * SUBPIXEL_POSITIONS + subpixel)
    }

    /// Ids for many glyphs under one lock: `f` gets an interning function.
    pub fn with<R>(&self, f: impl FnOnce(&mut dyn FnMut(GlyphKey, u32) -> GlyphId) -> R) -> R {
        let mut i = self.inner.lock();
        f(&mut |key, subpixel| GlyphId(Self::intern(&mut i, key) * SUBPIXEL_POSITIONS + subpixel))
    }

    fn intern(i: &mut Interner, key: GlyphKey) -> u32 {
        if let Some(&k) = i.map.get(&key) {
            return k;
        }
        let k = i.keys.len() as u32;
        i.keys.push(key);
        i.map.insert(key, k);
        k
    }

    /// The key and subpixel position of `id`, if it was handed out.
    pub fn key(&self, id: GlyphId) -> Option<(GlyphKey, u32)> {
        let i = self.inner.lock();
        i.keys
            .get((id.0 / SUBPIXEL_POSITIONS) as usize)
            .map(|&k| (k, id.subpixel()))
    }

    /// Ids handed out so far: every id is below this.
    pub fn len(&self) -> u32 {
        self.inner.lock().keys.len() as u32 * SUBPIXEL_POSITIONS
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// How large the cache is.
#[derive(Clone, Copy, Debug)]
pub struct GlyphCacheConfig {
    /// Side of a page in pixels (a layer of the cache's texture array).
    pub page_size: u32,
    pub max_pages: u32,
    /// Empty pixels kept around each glyph.
    pub gutter: u32,
}

impl Default for GlyphCacheConfig {
    /// 2048² `R8` pages (4 MiB each), up to 4.
    fn default() -> Self {
        Self {
            page_size: 2048,
            max_pages: 4,
            gutter: 1,
        }
    }
}

/// Where a resident glyph is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Placement {
    pub page: u32,
    /// In the page; an empty glyph (a space) has no size and no page area.
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// The bitmap's top-left relative to the pen at the baseline, whole pixels (y down).
    pub left: i32,
    pub top: i32,
}

#[derive(Clone, Copy, Default)]
struct Slot {
    placement: Option<Placement>,
    /// The last frame that drew it.
    used: u64,
}

struct Page {
    packer: Skyline,
    glyphs: Vec<GlyphId>,
    used: u64,
}

/// New bitmap data for a page.
#[derive(Clone, PartialEq, Debug)]
pub struct GlyphUpload {
    pub page: u32,
    pub rect: Rect,
    /// `rect.width * rect.height` coverage values, rows top to bottom.
    pub coverage: Vec<u8>,
}

/// What a [`GlyphCache::prepare`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PrepareStats {
    pub rasterized: u32,
    pub evicted_pages: u32,
    /// Glyphs that found no room even after evicting every page the frame does not use: they
    /// draw nothing this frame.
    pub dropped: u32,
}

/// A glyph's record in the GPU table ([`GlyphCache::table`]), four words:
/// `x | y << 16`, `width | height << 16`, `left | top << 16` (16-bit two's complement), and
/// `page`. Glyphs that are not resident have zero size.
pub type GlyphRecord = [u32; 4];

pub struct GlyphCache {
    pub ids: GlyphIds,
    config: GlyphCacheConfig,
    pages: Vec<Page>,
    slots: Vec<Slot>,
    frame: u64,
    uploads: Vec<GlyphUpload>,
    /// Bumped whenever a page is evicted: placements changed.
    generation: u64,
}

impl GlyphCache {
    pub fn new(config: GlyphCacheConfig) -> Self {
        assert!(config.page_size > 0 && config.page_size <= 1 << 15 && config.max_pages > 0);
        Self {
            ids: GlyphIds::new(),
            config,
            pages: Vec::new(),
            slots: Vec::new(),
            frame: 0,
            uploads: Vec::new(),
            generation: 0,
        }
    }

    pub fn config(&self) -> &GlyphCacheConfig {
        &self.config
    }

    /// Pages in use: the layers the cache's texture needs.
    pub fn page_count(&self) -> u32 {
        self.pages.len() as u32
    }

    /// Changes whenever placements of resident glyphs change (a page was evicted).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Where `id` is, if it is resident.
    pub fn placement(&self, id: GlyphId) -> Option<Placement> {
        self.slots.get(id.0 as usize).and_then(|s| s.placement)
    }

    /// Occupancy of each page, 0–1.
    pub fn page_occupancy(&self) -> Vec<f32> {
        let side = self.config.page_size as f32;
        let area = side * side;
        self.pages
            .iter()
            .map(|p| p.packer.used() as f32 / area)
            .collect()
    }

    /// Makes every glyph in `used` resident for a new frame, rasterizing the ones that are not
    /// (on `jobs` if given). `fonts[i]` is [`FontId`]`(i)`.
    pub fn prepare(
        &mut self,
        fonts: &[FontView<'_>],
        used: &[GlyphId],
        jobs: Option<&JobSystem>,
    ) -> PrepareStats {
        self.frame += 1;
        let frame = self.frame;
        let count = self.ids.len() as usize;
        if self.slots.len() < count {
            self.slots.resize(count, Slot::default());
        }
        let mut missing = Vec::new();
        for &id in used {
            let Some(slot) = self.slots.get_mut(id.0 as usize) else {
                continue;
            };
            if slot.used == frame {
                continue;
            }
            slot.used = frame;
            match slot.placement {
                Some(p) if p.width > 0 => self.pages[p.page as usize].used = frame,
                Some(_) => {}
                None => missing.push(id),
            }
        }
        let mut stats = PrepareStats::default();
        if missing.is_empty() {
            return stats;
        }
        missing.sort_unstable();
        let jobs_keys: Vec<_> = missing
            .iter()
            .map(|&id| self.ids.key(id).expect("an id that was handed out"))
            .collect();
        let raster = |i: usize| -> GlyphBitmap {
            let (key, subpixel) = jobs_keys[i];
            match fonts.get(key.font.0 as usize) {
                Some(font) => rasterize(font, key.glyph, key.size_px(), subpixel),
                None => GlyphBitmap::default(),
            }
        };
        let bitmaps: Vec<GlyphBitmap> = match jobs {
            Some(jobs) if missing.len() > 8 => {
                let out: Vec<Mutex<GlyphBitmap>> = (0..missing.len())
                    .map(|_| Mutex::new(GlyphBitmap::default()))
                    .collect();
                jobs.parallel_for(0..missing.len(), 8, |i| *out[i].lock() = raster(i));
                out.into_iter().map(Mutex::into_inner).collect()
            }
            _ => (0..missing.len()).map(raster).collect(),
        };
        stats.rasterized = missing.len() as u32;
        for (&id, bitmap) in missing.iter().zip(bitmaps) {
            if bitmap.width == 0 {
                self.slots[id.0 as usize].placement = Some(Placement::default());
                continue;
            }
            match self.place(&bitmap, &mut stats) {
                Some((page, rect)) => {
                    self.pages[page as usize].glyphs.push(id);
                    self.pages[page as usize].used = frame;
                    self.slots[id.0 as usize].placement = Some(Placement {
                        page,
                        x: rect.x,
                        y: rect.y,
                        width: bitmap.width,
                        height: bitmap.height,
                        left: bitmap.left,
                        top: bitmap.top,
                    });
                    self.uploads.push(GlyphUpload {
                        page,
                        rect: Rect {
                            width: bitmap.width,
                            height: bitmap.height,
                            ..rect
                        },
                        coverage: bitmap.coverage,
                    });
                }
                None => {
                    stats.dropped += 1;
                    // Not resident: tried again next frame.
                    self.slots[id.0 as usize].used = 0;
                }
            }
        }
        stats
    }

    /// Finds room for `bitmap`: a page that has it, a new page, or the least recently used page
    /// the frame does not draw from, evicted.
    fn place(&mut self, bitmap: &GlyphBitmap, stats: &mut PrepareStats) -> Option<(u32, Rect)> {
        let g = self.config.gutter;
        let (w, h) = (bitmap.width + 2 * g, bitmap.height + 2 * g);
        let inner = |r: Rect| Rect {
            x: r.x + g,
            y: r.y + g,
            width: bitmap.width,
            height: bitmap.height,
        };
        for (i, page) in self.pages.iter_mut().enumerate() {
            if let Some(r) = page.packer.insert(w, h) {
                return Some((i as u32, inner(r)));
            }
        }
        let size = self.config.page_size;
        if (self.pages.len() as u32) < self.config.max_pages {
            let mut packer = Skyline::new(size, size);
            let r = packer.insert(w, h)?;
            self.pages.push(Page {
                packer,
                glyphs: Vec::new(),
                used: 0,
            });
            return Some((self.pages.len() as u32 - 1, inner(r)));
        }
        let frame = self.frame;
        let (lru, _) = self
            .pages
            .iter()
            .enumerate()
            .filter(|(_, p)| p.used < frame)
            .min_by_key(|(_, p)| p.used)?;
        let page = &mut self.pages[lru];
        for id in page.glyphs.drain(..) {
            self.slots[id.0 as usize].placement = None;
        }
        page.packer.clear();
        self.generation += 1;
        stats.evicted_pages += 1;
        let r = page.packer.insert(w, h)?;
        Some((lru as u32, inner(r)))
    }

    /// The bitmaps rasterized since the last call, for the caller to copy into the pages.
    pub fn take_uploads(&mut self) -> Vec<GlyphUpload> {
        core::mem::take(&mut self.uploads)
    }

    /// Every id's [`GlyphRecord`], indexed by id: the GPU's id → page rectangle table.
    pub fn table(&self) -> Vec<GlyphRecord> {
        let count = (self.ids.len() as usize).max(1);
        let mut table = vec![[0u32; 4]; count];
        for (record, slot) in table.iter_mut().zip(&self.slots) {
            if let Some(p) = slot.placement {
                *record = [
                    p.x | p.y << 16,
                    p.width | p.height << 16,
                    (p.left as u16 as u32) | (p.top as u16 as u32) << 16,
                    p.page,
                ];
            }
        }
        table
    }
}
