//! The cook pipeline's font cooker: a [`Font`] source object in, a cooked
//! [`CookedFont`](marlin_font::CookedFont) out (see `marlin_font::asset` for the layout).
//!
//! The font file is parsed here ([`ttf`]); the runtime reads only the cooked form. TrueType's
//! points become closed contours of quadratic segments in half font units: implied on-curve
//! points (halfway between two off-curve points) are made explicit, straight segments get their
//! start as control point, and composite glyphs are flattened, their components' points rounded
//! to whole font units after the component transform.

pub mod ttf;

use marlin_asset::cooked::Encoding;
use marlin_cook_core::{CookContext, Cooked, Cooker, SourceObject};
use marlin_font::asset::{CmapRange, Contour, CookedFont, Font, GlyphEntry};
use marlin_reflect::type_info;
use marlin_serial::cooked::{CookedBuilder, RelSlice};

pub use ttf::{ParsedFont, ParsedGlyph};

/// Cooks [`Font`] objects.
pub struct FontCooker;

impl Cooker for FontCooker {
    fn version(&self) -> u32 {
        1
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let font: &Font = object
            .value
            .as_ref()
            .downcast()
            .ok_or("the object is not a Font")?;
        if font.source.path.is_empty() {
            return Err("`Source` is not set".into());
        }
        let bytes = ctx.read_source(&font.source.path)?;
        Ok(Cooked {
            type_id: type_info::<Font>().id,
            encoding: Encoding::Cooked,
            payload: cook_font(&bytes, font.index)?,
        })
    }
}

/// The cooked payload of face `index` of a font file: what [`FontCooker`] writes, for tools and
/// tests that cook a font without a project.
pub fn cook_font(file: &[u8], index: u32) -> Result<Vec<u8>, String> {
    build(&ttf::parse(file, index)?)
}

/// The cooked payload of a parsed font.
pub fn build(font: &ParsedFont) -> Result<Vec<u8>, String> {
    let mut glyphs = Vec::with_capacity(font.glyphs.len());
    let mut contours = Vec::new();
    let mut points: Vec<[i16; 2]> = Vec::new();
    for (g, glyph) in font.glyphs.iter().enumerate() {
        let first_contour = contours.len() as u32;
        for contour in &glyph.contours {
            let first = points.len() as u32;
            let segments =
                contour_points(contour, &mut points).map_err(|e| format!("glyph {g}: {e}"))?;
            if segments > 0 {
                contours.push(Contour { first, segments });
            }
        }
        let count = contours.len() as u32 - first_contour;
        let mut entry = GlyphEntry {
            advance: i16::try_from(glyph.advance)
                .map_err(|_| format!("glyph {g}: advance too large"))?,
            left_bearing: glyph.left_bearing,
            contour_count: u16::try_from(count)
                .map_err(|_| format!("glyph {g}: too many contours"))?,
            first_contour,
            ..GlyphEntry::default()
        };
        if count > 0 {
            let used = &points[contours[first_contour as usize].first as usize..];
            let (mut x0, mut y0, mut x1, mut y1) = (i16::MAX, i16::MAX, i16::MIN, i16::MIN);
            for &[x, y] in used {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
            // Half units to font units, outward.
            entry.x_min = x0.div_euclid(2);
            entry.y_min = y0.div_euclid(2);
            entry.x_max = (x1 + 1).div_euclid(2);
            entry.y_max = (y1 + 1).div_euclid(2);
        }
        glyphs.push(entry);
    }

    let mut cmap: Vec<CmapRange> = Vec::new();
    for (&c, &g) in &font.cmap {
        match cmap.last_mut() {
            Some(r) if r.last + 1 == c && r.glyph + (c - r.first) == g => r.last = c,
            _ => cmap.push(CmapRange {
                first: c,
                last: c,
                glyph: g,
            }),
        }
    }

    let mut b = CookedBuilder::new();
    let root = b.push(&CookedFont {
        units_per_em: font.units_per_em,
        glyph_count: font.glyphs.len() as u32,
        ascender: font.ascender,
        descender: font.descender,
        line_gap: font.line_gap,
        cap_height: font.cap_height,
        x_height: font.x_height,
        missing: 0,
        cmap: RelSlice::EMPTY,
        glyphs: RelSlice::EMPTY,
        contours: RelSlice::EMPTY,
        points: RelSlice::EMPTY,
        name: RelSlice::EMPTY,
    });
    let link = |e: marlin_serial::cooked::LayoutError| e.to_string();
    let s = b.push_slice(&cmap);
    b.link_slice(
        root.field(core::mem::offset_of!(CookedFont, cmap), |f| &f.cmap),
        s,
    )
    .map_err(link)?;
    let s = b.push_slice(&glyphs);
    b.link_slice(
        root.field(core::mem::offset_of!(CookedFont, glyphs), |f| &f.glyphs),
        s,
    )
    .map_err(link)?;
    let s = b.push_slice(&contours);
    b.link_slice(
        root.field(core::mem::offset_of!(CookedFont, contours), |f| &f.contours),
        s,
    )
    .map_err(link)?;
    let s = b.push_slice(&points);
    b.link_slice(
        root.field(core::mem::offset_of!(CookedFont, points), |f| &f.points),
        s,
    )
    .map_err(link)?;
    let s = b.push_str(&font.name);
    b.link_slice(
        root.field(core::mem::offset_of!(CookedFont, name), |f| &f.name),
        s,
    )
    .map_err(link)?;
    Ok(b.bytes().to_vec())
}

/// Appends one TrueType contour's points (font units, with on-curve flags) as a cooked contour
/// in half units; returns its segment count (0 for a degenerate contour, which adds nothing).
fn contour_points(contour: &[(f64, f64, bool)], out: &mut Vec<[i16; 2]>) -> Result<u32, String> {
    let half = |v: f64| -> Result<i32, String> {
        let h = 2 * v.round() as i64;
        if (i64::from(i16::MIN)..=i64::from(i16::MAX)).contains(&h) {
            Ok(h as i32)
        } else {
            Err(format!("coordinate {v} is outside ±16383 font units"))
        }
    };
    let mut pts = Vec::with_capacity(contour.len() + 1);
    for &(x, y, on) in contour {
        pts.push(([half(x)?, half(y)?], on));
    }
    // Start at an on-curve point; a contour of off-curve points only starts between its first two.
    let start = match pts.iter().position(|p| p.1) {
        Some(i) => {
            pts.rotate_left(i);
            pts[0].0
        }
        None => {
            let [a, b] = [pts[0].0, pts[1].0];
            let mid = [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2];
            // mid, 1, 2, …, 0: point 0 comes last, as the control point of the closing segment.
            pts.rotate_left(1);
            pts.insert(0, (mid, true));
            mid
        }
    };
    // (control, end) pairs; the last ends at the start.
    let mut segments: Vec<([i32; 2], [i32; 2])> = Vec::new();
    let mut current = start;
    let mut control: Option<[i32; 2]> = None;
    for &(p, on) in pts[1..].iter().chain(core::iter::once(&(start, true))) {
        if on {
            let c = control.take().unwrap_or(current);
            if p != current || c != current {
                segments.push((c, p));
            }
            current = p;
        } else if let Some(c) = control {
            let mid = [(c[0] + p[0]) / 2, (c[1] + p[1]) / 2];
            segments.push((c, mid));
            current = mid;
            control = Some(p);
        } else {
            control = Some(p);
        }
    }
    if segments.is_empty() {
        // A single point: no area.
        return Ok(0);
    }
    let narrow = |p: [i32; 2]| [p[0] as i16, p[1] as i16];
    out.push(narrow(start));
    let n = segments.len();
    for (i, &(c, e)) in segments.iter().enumerate() {
        out.push(narrow(c));
        if i + 1 < n {
            out.push(narrow(e));
        }
    }
    // The last segment must end where the contour starts.
    debug_assert!(segments.last().is_none_or(|s| s.1 == start));
    Ok(n as u32)
}
