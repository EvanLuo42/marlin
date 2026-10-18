//! Fonts as assets: the source settings authored in `.masset` text and the cooked form the
//! runtime reads in place (docs/design/0012-ui-renderer.md §6, U1 record).
//!
//! The cooker parses the font file (TrueType outlines) and keeps what the runtime needs: the
//! vertical metrics, the character map as ranges, each glyph's advance and bounding box, and
//! its outline as closed contours of quadratic segments. The runtime never parses a font file.
//!
//! Outline coordinates are in **half font units** (`i16`): TrueType's implied on-curve points lie
//! halfway between two off-curve points, so half units hold every point exactly. A contour of `n`
//! segments is `2n` points: segment `s` starts at point `2s`, has its control point at `2s + 1`
//! and ends at point `2s + 2`, the last one back at point 0. A straight segment has its control point equal to
//! its start, which is a straight line as a quadratic too.

use marlin_asset::SourceFile;
use marlin_reflect::Reflect;
use marlin_serial::cooked::{CookedView, LayoutError, Pod, RelSlice};

/// A font asset: a font file and how to cook it.
///
/// ```text
/// Font
/// {
///     !Uuid = "…"
///     !Name = "Marlin.Engine.Fonts.Inter"
///     Source = `source:fonts/Inter-Regular.ttf`
/// }
/// ```
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "marlin_font::Font", default)]
pub struct Font {
    /// A TrueType font (`.ttf`, glyf outlines). A collection (`.ttc`) takes [`index`](Self::index).
    pub source: SourceFile,
    /// The face in a collection; 0 otherwise.
    pub index: u32,
}

/// The payload root of a cooked font.
#[derive(Clone, Copy, Debug, Pod)]
#[repr(C)]
pub struct CookedFont {
    pub units_per_em: u32,
    pub glyph_count: u32,
    /// Font units, y up: the typographic ascender (positive) and descender (negative), and the
    /// gap between lines.
    pub ascender: i32,
    pub descender: i32,
    pub line_gap: i32,
    /// Font units; 0 when the font does not say.
    pub cap_height: i32,
    pub x_height: i32,
    /// The glyph for characters the map lacks (glyph 0, `.notdef`, in every TrueType font).
    pub missing: u32,
    /// Sorted, disjoint.
    pub cmap: RelSlice<CmapRange>,
    /// One per glyph.
    pub glyphs: RelSlice<GlyphEntry>,
    pub contours: RelSlice<Contour>,
    /// Half font units.
    pub points: RelSlice<[i16; 2]>,
    /// The family and style, for messages.
    pub name: RelSlice<u8>,
}

/// Characters `first..=last` map to glyphs `glyph..=glyph + (last - first)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Pod)]
#[repr(C)]
pub struct CmapRange {
    pub first: u32,
    pub last: u32,
    pub glyph: u32,
}

/// One glyph's metrics and where its outline is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Pod)]
#[repr(C)]
pub struct GlyphEntry {
    /// Font units.
    pub advance: i16,
    pub left_bearing: i16,
    /// Font units, y up: the outline's bounding box; all zero for an empty glyph.
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
    pub contour_count: u16,
    pub spare: u16,
    pub first_contour: u32,
}

/// A closed contour: `points[first..first + 2 * segments]`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Pod)]
#[repr(C)]
pub struct Contour {
    pub first: u32,
    pub segments: u32,
}

/// Errors of a cooked font beyond what the layout view checks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FontError {
    Layout(LayoutError),
    /// Counts or ranges that do not describe a font.
    Malformed,
}

impl From<LayoutError> for FontError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// A checked cooked font.
#[derive(Clone, Copy, Debug)]
pub struct FontView<'a> {
    pub header: &'a CookedFont,
    pub cmap: &'a [CmapRange],
    pub glyphs: &'a [GlyphEntry],
    pub contours: &'a [Contour],
    pub points: &'a [[i16; 2]],
    pub name: &'a str,
}

/// A quadratic segment in font units (y up): from `p0` through control `c` to `p1`. Straight
/// when `c == p0`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Segment {
    pub p0: [f32; 2],
    pub c: [f32; 2],
    pub p1: [f32; 2],
}

impl Segment {
    pub fn is_line(&self) -> bool {
        self.c == self.p0
    }
}

impl<'a> FontView<'a> {
    /// Reads `payload` and checks that every glyph's contours and every contour's points lie in
    /// their tables and the character map is sorted, so lookups cannot fail later.
    pub fn new(payload: &'a [u8]) -> Result<Self, FontError> {
        let view = CookedView::new(payload);
        let header: &CookedFont = view.root()?;
        let cmap = view.slice(&header.cmap)?;
        let glyphs = view.slice(&header.glyphs)?;
        let contours = view.slice(&header.contours)?;
        let points = view.slice(&header.points)?;
        let name = view.str(&header.name)?;
        let malformed = header.units_per_em == 0
            || glyphs.len() != header.glyph_count as usize
            || header.missing >= header.glyph_count
            || cmap.windows(2).any(|w| w[0].last >= w[1].first)
            || cmap.iter().any(|r| {
                r.first > r.last
                    || u64::from(r.glyph) + u64::from(r.last - r.first)
                        >= u64::from(header.glyph_count)
            })
            || glyphs
                .iter()
                .any(|g| g.first_contour as usize + g.contour_count as usize > contours.len())
            || contours
                .iter()
                .any(|c| c.first as u64 + 2 * c.segments as u64 > points.len() as u64);
        if malformed {
            return Err(FontError::Malformed);
        }
        Ok(Self {
            header,
            cmap,
            glyphs,
            contours,
            points,
            name,
        })
    }

    pub fn units_per_em(&self) -> f32 {
        self.header.units_per_em as f32
    }

    /// The glyph of `c`, or `None` if the font has none.
    pub fn glyph_of(&self, c: char) -> Option<u32> {
        let c = c as u32;
        let i = self.cmap.partition_point(|r| r.last < c);
        let r = self.cmap.get(i)?;
        (r.first <= c).then(|| r.glyph + (c - r.first))
    }

    /// The glyph of `c`, or the font's missing glyph.
    pub fn glyph_or_missing(&self, c: char) -> u32 {
        self.glyph_of(c).unwrap_or(self.header.missing)
    }

    /// `glyph`'s entry; glyph ids past the font's are the missing glyph's.
    pub fn glyph(&self, glyph: u32) -> &'a GlyphEntry {
        self.glyphs
            .get(glyph as usize)
            .unwrap_or(&self.glyphs[self.header.missing as usize])
    }

    /// The advance of `glyph` in font units.
    pub fn advance(&self, glyph: u32) -> f32 {
        f32::from(self.glyph(glyph).advance)
    }

    /// `glyph`'s outline segments in font units (y up), contour after contour.
    pub fn segments(&self, glyph: u32) -> impl Iterator<Item = Segment> + 'a {
        let g = self.glyph(glyph);
        let points = self.points;
        let first = g.first_contour as usize;
        self.contours[first..first + g.contour_count as usize]
            .iter()
            .flat_map(move |c| {
                let base = c.first as usize;
                let p = move |i: usize| {
                    let [x, y] = points[base + i];
                    [f32::from(x) * 0.5, f32::from(y) * 0.5]
                };
                let n = c.segments as usize;
                (0..n).map(move |s| Segment {
                    p0: p(2 * s),
                    c: p(2 * s + 1),
                    p1: if s + 1 == n { p(0) } else { p(2 * s + 2) },
                })
            })
    }
}
