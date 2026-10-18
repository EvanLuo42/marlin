//! Text layout: advances, line breaks and the box.

use marlin_font::{FontId, TextLayout};

use crate::inter;

#[test]
fn glyphs_advance_and_lines_break() {
    let font = inter();
    let size = 20.0;
    let s = size / font.units_per_em();
    let layout = TextLayout::new(&font, FontId(0), size, "AV\nW");
    assert_eq!(layout.glyphs.len(), 3);
    let a = font.glyph_of('A').unwrap();
    let v = font.glyph_of('V').unwrap();
    let w = font.glyph_of('W').unwrap();
    assert_eq!(layout.glyphs[0].glyph, a);
    assert_eq!(layout.glyphs[0].x, 0.0);
    assert_eq!(layout.glyphs[1].x, font.advance(a) * s);
    assert_eq!(layout.glyphs[0].y, layout.ascent);
    assert_eq!(layout.glyphs[2].x, 0.0);
    assert_eq!(layout.glyphs[2].y, layout.ascent + layout.line_height);
    assert_eq!(
        layout.width,
        (font.advance(a) + font.advance(v)).max(font.advance(w)) * s
    );
    assert_eq!(layout.height, 2.0 * layout.line_height);
    let h = font.header;
    assert_eq!(layout.ascent, h.ascender as f32 * s);
    assert!(layout.line_height > size);
    // Characters the font lacks are its missing glyph.
    let missing = TextLayout::new(&font, FontId(0), size, "\u{10FFFD}");
    assert_eq!(missing.glyphs[0].glyph, 0);
}
