//! Text layout, U1's minimal one: characters to glyphs through the font's character map, placed
//! by their advances, lines broken at `\n`. No shaping (ligatures, kerning, bidi, complex
//! scripts): that is `marlin-font`'s shaping design (0012 §6.5, §15), still open.

use alloc::vec::Vec;

use crate::asset::FontView;
use crate::cache::FontId;

/// A glyph of a layout, at its pen position: the baseline, relative to the layout's top-left,
/// in the layout's units (logical pixels).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PositionedGlyph {
    pub glyph: u32,
    pub x: f32,
    pub y: f32,
}

/// Laid-out text: its glyphs and its box.
#[derive(Clone, PartialEq, Debug)]
pub struct TextLayout {
    pub font: FontId,
    /// Pixels per em, logical.
    pub size: f32,
    pub glyphs: Vec<PositionedGlyph>,
    /// The widest line's advance.
    pub width: f32,
    /// Lines times the line height.
    pub height: f32,
    /// From a line's top to its baseline.
    pub ascent: f32,
    pub line_height: f32,
}

impl TextLayout {
    /// Lays out `text` in `font` (known to the glyph cache as `id`) at `size` px per em.
    pub fn new(font: &FontView<'_>, id: FontId, size: f32, text: &str) -> Self {
        let s = size / font.units_per_em();
        let h = font.header;
        let ascent = h.ascender as f32 * s;
        let line_height = (h.ascender - h.descender + h.line_gap) as f32 * s;
        let mut glyphs = Vec::with_capacity(text.len());
        let (mut x, mut y, mut width) = (0.0f32, ascent, 0.0f32);
        let mut lines = 1;
        for c in text.chars() {
            if c == '\n' {
                width = width.max(x);
                x = 0.0;
                y += line_height;
                lines += 1;
                continue;
            }
            let glyph = font.glyph_or_missing(c);
            glyphs.push(PositionedGlyph { glyph, x, y });
            x += font.advance(glyph) * s;
        }
        Self {
            font: id,
            size,
            glyphs,
            width: width.max(x),
            height: lines as f32 * line_height,
            ascent,
            line_height,
        }
    }
}
