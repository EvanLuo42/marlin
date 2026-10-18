//! Recording: the [`Painter`] writes commands into a [`UiDrawList`] (0012 §2).
//!
//! A draw list is an ordered sequence of chunks. [`Painter::chunk`] reserves the next place in
//! the order and returns a [`ChunkSlot`], a list of its own that another job can record into;
//! [`UiDrawList::fill`] puts it back in its place. A recorded chunk can be kept as a
//! [`RetainedChunk`] and replayed in a later frame at an offset: a copy of its words, with the
//! coordinates moved. Replay offsets snap to whole physical pixels, so text keeps its subpixel
//! positions (and with them its glyph ids) and stays as crisp as it was recorded.

use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_font::{GlyphIds, GlyphKey, SUBPIXEL_POSITIONS, TextLayout};
use marlin_math::Vec2;
use marlin_math::scalar::{floor, round};

use crate::command::{Chunk, DEFAULT_SAMPLER, IMAGE_SRGB_PREMULTIPLIED, Kind, OPAQUE, f};
use crate::style::{
    Color, Corners, ImageColor, ImageSource, ImageStyle, LineCap, Rect, RectStyle, ShapeStyle,
    StrokeStyle,
};

enum Part {
    Own(Chunk),
    /// A slot handed out and not filled yet: draws nothing.
    Reserved,
    Filled(UiDrawList),
}

/// One surface's commands for a frame, in drawing order.
pub struct UiDrawList {
    /// Physical pixels per logical pixel.
    scale: f32,
    parts: Vec<Part>,
}

impl UiDrawList {
    /// An empty list for a surface with `scale` physical pixels per logical pixel (its DPI
    /// scale).
    pub fn new(scale: f32) -> Self {
        Self {
            scale,
            parts: Vec::new(),
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// A painter appending to the list; `ids` gives glyphs their ids.
    pub fn painter<'a>(&'a mut self, ids: &'a GlyphIds) -> Painter<'a> {
        Painter { list: self, ids }
    }

    /// Puts a slot's recording in the place [`Painter::chunk`] reserved for it.
    pub fn fill(&mut self, slot: ChunkSlot) {
        match self.parts.get_mut(slot.index) {
            Some(p @ Part::Reserved) => *p = Part::Filled(slot.list),
            _ => panic!("the slot is not this list's, or it was filled already"),
        }
    }

    fn own(&mut self) -> &mut Chunk {
        if !matches!(self.parts.last(), Some(Part::Own(_))) {
            self.parts.push(Part::Own(Chunk::default()));
        }
        match self.parts.last_mut() {
            Some(Part::Own(c)) => c,
            _ => unreachable!(),
        }
    }

    /// Commands recorded, slots included.
    pub fn len(&self) -> usize {
        self.parts
            .iter()
            .map(|p| match p {
                Part::Own(c) => c.len(),
                Part::Reserved => 0,
                Part::Filled(l) => l.len(),
            })
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every command in order, as one chunk: what the renderer uploads.
    pub fn flatten(&self) -> Chunk {
        let mut out = Chunk::default();
        self.flatten_into(&mut out);
        out
    }

    pub fn flatten_into(&self, out: &mut Chunk) {
        for p in &self.parts {
            match p {
                Part::Own(c) => out.append(c),
                Part::Reserved => {}
                Part::Filled(l) => l.flatten_into(out),
            }
        }
    }
}

/// A place in a draw list's order, recorded elsewhere (on another job) and put back with
/// [`UiDrawList::fill`].
pub struct ChunkSlot {
    index: usize,
    list: UiDrawList,
}

impl ChunkSlot {
    pub fn painter<'a>(&'a mut self, ids: &'a GlyphIds) -> Painter<'a> {
        self.list.painter(ids)
    }

    /// What has been recorded, kept for replaying in later frames.
    pub fn retain(&self) -> RetainedChunk {
        RetainedChunk {
            chunk: Arc::new(self.list.flatten()),
            scale: self.list.scale,
        }
    }
}

/// A recorded chunk kept across frames (§2.3).
#[derive(Clone)]
pub struct RetainedChunk {
    chunk: Arc<Chunk>,
    scale: f32,
}

impl RetainedChunk {
    /// Records a chunk for a surface of `scale`.
    pub fn record(scale: f32, ids: &GlyphIds, f: impl FnOnce(&mut Painter<'_>)) -> Self {
        let mut list = UiDrawList::new(scale);
        f(&mut list.painter(ids));
        Self {
            chunk: Arc::new(list.flatten()),
            scale,
        }
    }

    pub fn len(&self) -> usize {
        self.chunk.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chunk.is_empty()
    }
}

/// Writes commands (§2.1). Coordinates are logical pixels; the surface's scale is applied as
/// they are written.
pub struct Painter<'a> {
    list: &'a mut UiDrawList,
    ids: &'a GlyphIds,
}

impl Painter<'_> {
    fn s(&self, v: f32) -> u32 {
        f(v * self.list.scale)
    }

    fn corners(&self, c: &Corners) -> [u32; 4] {
        [c.top_left, c.top_right, c.bottom_right, c.bottom_left].map(|r| self.s(r.max(0.0)))
    }

    /// A rectangle: filled, rounded per corner, optionally outlined.
    pub fn rect(&mut self, r: Rect, style: &RectStyle) {
        let (sw, sc) = style
            .stroke
            .map_or((0.0, Color::TRANSPARENT), |s| (s.width.max(0.0), s.color));
        let opaque = style.fill.a == 255 && (sw == 0.0 || sc.a == 255);
        let [r0, r1, r2, r3] = self.corners(&style.corners);
        let payload = [
            self.s(r.min.x),
            self.s(r.min.y),
            self.s(r.max.x),
            self.s(r.max.y),
            r0,
            r1,
            r2,
            r3,
            self.s(sw),
            sc.packed(),
        ];
        let chunk = self.list.own();
        let paint = chunk.solid(style.fill);
        chunk.push(Kind::Rect, if opaque { OPAQUE } else { 0 }, paint, &payload);
    }

    pub fn circle(&mut self, center: Vec2, radius: f32, style: &ShapeStyle) {
        let (sw, sc) = style
            .stroke
            .map_or((0.0, Color::TRANSPARENT), |s| (s.width.max(0.0), s.color));
        let opaque = style.fill.a == 255 && (sw == 0.0 || sc.a == 255);
        let payload = [
            self.s(center.x),
            self.s(center.y),
            self.s(radius.max(0.0)),
            self.s(sw),
            sc.packed(),
        ];
        let chunk = self.list.own();
        let paint = chunk.solid(style.fill);
        chunk.push(
            Kind::Circle,
            if opaque { OPAQUE } else { 0 },
            paint,
            &payload,
        );
    }

    /// A line segment of `style.width`, with caps and dashes.
    pub fn line(&mut self, a: Vec2, b: Vec2, style: &StrokeStyle) {
        let cap = match style.cap {
            LineCap::Butt => 0,
            LineCap::Round => 1,
            LineCap::Square => 2,
        };
        let (dash, gap, phase) = style
            .dash
            .filter(|d| d.dash > 0.0 && d.gap > 0.0)
            .map_or((0.0, 0.0, 0.0), |d| (d.dash, d.gap, d.phase));
        let payload = [
            self.s(a.x),
            self.s(a.y),
            self.s(b.x),
            self.s(b.y),
            self.s(style.width.max(0.0)),
            cap,
            self.s(dash),
            self.s(gap),
            self.s(phase),
        ];
        let chunk = self.list.own();
        let paint = chunk.solid(style.color);
        chunk.push(Kind::Line, 0, paint, &payload);
    }

    pub fn triangle(&mut self, p: [Vec2; 3], style: &ShapeStyle) {
        let (sw, sc) = style
            .stroke
            .map_or((0.0, Color::TRANSPARENT), |s| (s.width.max(0.0), s.color));
        let payload = [
            self.s(p[0].x),
            self.s(p[0].y),
            self.s(p[1].x),
            self.s(p[1].y),
            self.s(p[2].x),
            self.s(p[2].y),
            self.s(sw),
            sc.packed(),
        ];
        let chunk = self.list.own();
        let paint = chunk.solid(style.fill);
        chunk.push(Kind::Triangle, 0, paint, &payload);
    }

    /// Laid-out text with its top-left at `origin`, rasterized at the surface's pixel size
    /// (§6.2): each glyph's pen is snapped to whole pixels vertically and to quarter pixels
    /// horizontally.
    pub fn text(&mut self, layout: &TextLayout, origin: Vec2, color: Color) {
        let scale = self.list.scale;
        let size = layout.size * scale;
        let ids = self.ids;
        let chunk = self.list.own();
        let paint = chunk.solid(color);
        ids.with(|id| {
            for g in &layout.glyphs {
                let x = (origin.x + g.x) * scale;
                let y = round((origin.y + g.y) * scale);
                let mut xi = floor(x);
                let mut q = round((x - xi) * SUBPIXEL_POSITIONS as f32) as u32;
                if q == SUBPIXEL_POSITIONS {
                    xi += 1.0;
                    q = 0;
                }
                let glyph = id(GlyphKey::new(layout.font, g.glyph, size), q);
                chunk.push(
                    Kind::Glyph,
                    0,
                    paint,
                    &[xi as i32 as u32, y as i32 as u32, glyph.0],
                );
            }
        });
    }

    /// A texture in `r`, through `style`'s UV rectangle, tint and corners.
    pub fn image(&mut self, r: Rect, src: ImageSource, style: &ImageStyle) {
        let ImageSource::Texture {
            view,
            width,
            height,
            color,
        } = src;
        let flags = match color {
            ImageColor::LinearStraight => 0,
            ImageColor::SrgbPremultiplied => IMAGE_SRGB_PREMULTIPLIED,
        };
        let [r0, r1, r2, r3] = self.corners(&style.corners);
        let payload = [
            self.s(r.min.x),
            self.s(r.min.y),
            self.s(r.max.x),
            self.s(r.max.y),
            f(style.uv.min.x),
            f(style.uv.min.y),
            f(style.uv.max.x),
            f(style.uv.max.y),
            view,
            style.sampler.unwrap_or(DEFAULT_SAMPLER),
            width.min(0xFFFF) | height.min(0xFFFF) << 16,
            r0,
            r1,
            r2,
            r3,
        ];
        let chunk = self.list.own();
        let paint = chunk.solid(style.tint);
        chunk.push(Kind::Image, flags, paint, &payload);
    }

    /// Reserves the next place in the order for a chunk recorded elsewhere.
    pub fn chunk(&mut self) -> ChunkSlot {
        let index = self.list.parts.len();
        self.list.parts.push(Part::Reserved);
        ChunkSlot {
            index,
            list: UiDrawList::new(self.list.scale),
        }
    }

    /// Inserts a retained chunk moved by `offset` (logical pixels, snapped to whole physical
    /// pixels). A chunk recorded for another scale is replayed unscaled.
    pub fn replay(&mut self, chunk: &RetainedChunk, offset: Vec2) {
        debug_assert_eq!(chunk.scale, self.list.scale, "replayed at another scale");
        let dx = round(offset.x * self.list.scale);
        let dy = round(offset.y * self.list.scale);
        let own = self.list.own();
        let base = own.payload.len();
        own.append(&chunk.chunk);
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        let start = own.commands.len() - chunk.chunk.len();
        for c in &own.commands[start..] {
            let at = c[3] as usize;
            debug_assert!(at >= base);
            let p = &mut own.payload[at..];
            let mv = |w: &mut u32, d: f32| *w = f(f32::from_bits(*w) + d);
            match Kind::from_word(c[0]) {
                Some(Kind::Rect | Kind::Line | Kind::Image) => {
                    mv(&mut p[0], dx);
                    mv(&mut p[1], dy);
                    mv(&mut p[2], dx);
                    mv(&mut p[3], dy);
                }
                Some(Kind::Circle) => {
                    mv(&mut p[0], dx);
                    mv(&mut p[1], dy);
                }
                Some(Kind::Triangle) => {
                    for k in 0..3 {
                        mv(&mut p[2 * k], dx);
                        mv(&mut p[2 * k + 1], dy);
                    }
                }
                Some(Kind::Glyph) => {
                    p[0] = (p[0] as i32 + dx as i32) as u32;
                    p[1] = (p[1] as i32 + dy as i32) as u32;
                }
                None => {}
            }
        }
    }
}
