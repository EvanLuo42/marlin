//! A TrueType reader for what the cooked font keeps: `head`, `maxp`, `hhea`, `hmtx`, `OS/2`,
//! `cmap` (formats 4 and 12), `loca`, `glyf` (simple and composite glyphs) and `name`.
//! Hinting instructions are skipped; CFF outlines are refused.

use std::collections::BTreeMap;

/// What the cooker reads from a font file.
#[derive(Debug, Default)]
pub struct ParsedFont {
    pub units_per_em: u32,
    pub ascender: i32,
    pub descender: i32,
    pub line_gap: i32,
    pub cap_height: i32,
    pub x_height: i32,
    /// Character → glyph, without glyph 0.
    pub cmap: BTreeMap<u32, u32>,
    pub glyphs: Vec<ParsedGlyph>,
    pub name: String,
}

#[derive(Debug, Default, Clone)]
pub struct ParsedGlyph {
    pub advance: u16,
    pub left_bearing: i16,
    /// Contours of TrueType points in font units: (x, y, on-curve).
    pub contours: Vec<Vec<(f64, f64, bool)>>,
}

struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn u8(&self, at: usize) -> Result<u8, String> {
        self.data.get(at).copied().ok_or_else(truncated)
    }
    fn u16(&self, at: usize) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.bytes::<2>(at)?))
    }
    fn i16(&self, at: usize) -> Result<i16, String> {
        Ok(i16::from_be_bytes(self.bytes::<2>(at)?))
    }
    fn u32(&self, at: usize) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.bytes::<4>(at)?))
    }
    fn bytes<const N: usize>(&self, at: usize) -> Result<[u8; N], String> {
        self.data
            .get(at..at + N)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(truncated)
    }
    fn slice(&self, at: usize, len: usize) -> Result<&'a [u8], String> {
        self.data.get(at..at + len).ok_or_else(truncated)
    }
}

fn truncated() -> String {
    "the font file is truncated or a table points outside it".into()
}

/// Parses face `index` of `data` (a `.ttf`, or a `.ttc` collection).
pub fn parse(data: &[u8], index: u32) -> Result<ParsedFont, String> {
    let file = Reader { data };
    let mut offset = 0usize;
    match file.u32(0)? {
        0x7474_6366 => {
            // 'ttcf'
            let count = file.u32(8)?;
            if index >= count {
                return Err(format!(
                    "the collection has {count} faces; `Index` is {index}"
                ));
            }
            offset = file.u32(12 + 4 * index as usize)? as usize;
        }
        _ if index != 0 => return Err("`Index` is for collections; this file has one face".into()),
        _ => {}
    }
    match file.u32(offset)? {
        0x0001_0000 | 0x7472_7565 => {}
        0x4f54_544f => {
            return Err(
                "CFF outlines (an OpenType `OTTO` font) are not supported yet; use a \
                        TrueType (glyf) build of the font"
                    .into(),
            );
        }
        tag => return Err(format!("not a TrueType font (tag {tag:#010x})")),
    }
    let tables = file.u16(offset + 4)? as usize;
    let mut dir = BTreeMap::new();
    for i in 0..tables {
        let at = offset + 12 + 16 * i;
        let tag = file.bytes::<4>(at)?;
        let start = file.u32(at + 8)? as usize;
        let len = file.u32(at + 12)? as usize;
        dir.insert(tag, file.slice(start, len)?);
    }
    let table = |tag: &[u8; 4]| -> Result<Reader<'_>, String> {
        dir.get(tag)
            .map(|&data| Reader { data })
            .ok_or_else(|| format!("the font has no `{}` table", String::from_utf8_lossy(tag)))
    };

    let head = table(b"head")?;
    let units_per_em = u32::from(head.u16(18)?);
    if !(16..=16384).contains(&units_per_em) {
        return Err(format!("unitsPerEm {units_per_em} is outside 16..=16384"));
    }
    let long_loca = head.i16(50)? != 0;
    let glyph_count = table(b"maxp")?.u16(4)? as usize;
    if glyph_count == 0 {
        return Err("the font has no glyphs".into());
    }
    let hhea = table(b"hhea")?;
    let mut font = ParsedFont {
        units_per_em,
        ascender: i32::from(hhea.i16(4)?),
        descender: i32::from(hhea.i16(6)?),
        line_gap: i32::from(hhea.i16(8)?),
        ..ParsedFont::default()
    };
    if let Ok(os2) = table(b"OS/2") {
        let version = os2.u16(0)?;
        // fsSelection bit 7: USE_TYPO_METRICS.
        if os2.u16(62)? & 0x80 != 0 {
            font.ascender = i32::from(os2.i16(68)?);
            font.descender = i32::from(os2.i16(70)?);
            font.line_gap = i32::from(os2.i16(72)?);
        }
        if version >= 2 {
            font.x_height = i32::from(os2.i16(86)?);
            font.cap_height = i32::from(os2.i16(88)?);
        }
    }

    // Horizontal metrics.
    let metrics = (hhea.u16(34)? as usize).clamp(1, glyph_count);
    let hmtx = table(b"hmtx")?;
    font.glyphs = vec![ParsedGlyph::default(); glyph_count];
    let mut last_advance = 0;
    for (g, glyph) in font.glyphs.iter_mut().enumerate() {
        if g < metrics {
            last_advance = hmtx.u16(4 * g)?;
            glyph.left_bearing = hmtx.i16(4 * g + 2)?;
        } else {
            // Fonts may leave the trailing bearings out.
            glyph.left_bearing = hmtx.i16(4 * metrics + 2 * (g - metrics)).unwrap_or(0);
        }
        glyph.advance = last_advance;
    }

    // Outlines.
    let loca = table(b"loca")?;
    let glyf = table(b"glyf")?;
    let ranges: Vec<(usize, usize)> = (0..glyph_count)
        .map(|g| -> Result<_, String> {
            Ok(if long_loca {
                (loca.u32(4 * g)? as usize, loca.u32(4 * g + 4)? as usize)
            } else {
                (
                    2 * loca.u16(2 * g)? as usize,
                    2 * loca.u16(2 * g + 2)? as usize,
                )
            })
        })
        .collect::<Result<_, _>>()?;
    for (g, glyph) in font.glyphs.iter_mut().enumerate() {
        glyph.contours = outline(&glyf, &ranges, g, 0).map_err(|e| format!("glyph {g}: {e}"))?;
    }

    font.cmap = cmap(&table(b"cmap")?, glyph_count)?;
    font.name = table(b"name")
        .ok()
        .and_then(|n| name(&n))
        .unwrap_or_default();
    Ok(font)
}

type Contours = Vec<Vec<(f64, f64, bool)>>;

/// Glyph `g`'s contours, components of composite glyphs placed.
fn outline(
    glyf: &Reader<'_>,
    ranges: &[(usize, usize)],
    g: usize,
    depth: u32,
) -> Result<Contours, String> {
    if depth > 8 {
        return Err("composite glyphs nest too deep".into());
    }
    let (start, end) = *ranges
        .get(g)
        .ok_or("a component names a glyph the font lacks")?;
    if end <= start {
        return Ok(Vec::new());
    }
    let r = Reader {
        data: glyf.slice(start, end - start)?,
    };
    let contours = r.i16(0)?;
    if contours >= 0 {
        simple(&r, contours as usize)
    } else {
        composite(glyf, ranges, &r, depth)
    }
}

fn simple(r: &Reader<'_>, contours: usize) -> Result<Contours, String> {
    let mut ends = Vec::with_capacity(contours);
    for i in 0..contours {
        ends.push(r.u16(10 + 2 * i)? as usize);
    }
    let points = ends.last().map_or(0, |&e| e + 1);
    if ends.windows(2).any(|w| w[1] < w[0]) {
        return Err("contour ends are not ascending".into());
    }
    let instructions = r.u16(10 + 2 * contours)? as usize;
    let mut at = 12 + 2 * contours + instructions;
    let mut flags = Vec::with_capacity(points);
    while flags.len() < points {
        let f = r.u8(at)?;
        at += 1;
        let mut repeat = 1;
        if f & 8 != 0 {
            repeat += r.u8(at)? as usize;
            at += 1;
        }
        for _ in 0..repeat {
            flags.push(f);
        }
    }
    flags.truncate(points);
    let mut coords = |short: u8, same: u8| -> Result<Vec<i32>, String> {
        let mut v = 0i32;
        let mut out = Vec::with_capacity(points);
        for &f in &flags {
            if f & short != 0 {
                let d = i32::from(r.u8(at)?);
                at += 1;
                v += if f & same != 0 { d } else { -d };
            } else if f & same == 0 {
                v += i32::from(r.i16(at)?);
                at += 2;
            }
            out.push(v);
        }
        Ok(out)
    };
    let xs = coords(2, 16)?;
    let ys = coords(4, 32)?;
    let mut out = Vec::with_capacity(contours);
    let mut first = 0;
    for &end in &ends {
        let contour: Vec<_> = (first..=end)
            .map(|i| (f64::from(xs[i]), f64::from(ys[i]), flags[i] & 1 != 0))
            .collect();
        first = end + 1;
        if contour.len() >= 2 {
            out.push(contour);
        }
    }
    Ok(out)
}

fn composite(
    glyf: &Reader<'_>,
    ranges: &[(usize, usize)],
    r: &Reader<'_>,
    depth: u32,
) -> Result<Contours, String> {
    let mut out = Vec::new();
    let mut at = 10;
    loop {
        let flags = r.u16(at)?;
        let glyph = r.u16(at + 2)? as usize;
        at += 4;
        let (a1, a2) = if flags & 1 != 0 {
            let v = (r.i16(at)?, r.i16(at + 2)?);
            at += 4;
            (f64::from(v.0), f64::from(v.1))
        } else {
            let v = (r.u8(at)? as i8, r.u8(at + 1)? as i8);
            at += 2;
            (f64::from(v.0), f64::from(v.1))
        };
        let f2dot14 = |at: usize| -> Result<f64, String> { Ok(f64::from(r.i16(at)?) / 16384.0) };
        let (mut a, mut b, mut c, mut d) = (1.0, 0.0, 0.0, 1.0);
        if flags & 8 != 0 {
            a = f2dot14(at)?;
            d = a;
            at += 2;
        } else if flags & 0x40 != 0 {
            a = f2dot14(at)?;
            d = f2dot14(at + 2)?;
            at += 4;
        } else if flags & 0x80 != 0 {
            a = f2dot14(at)?;
            b = f2dot14(at + 2)?;
            c = f2dot14(at + 4)?;
            d = f2dot14(at + 6)?;
            at += 8;
        }
        // ARGS_ARE_XY_VALUES; point matching (rare, and only in old fonts) places the
        // component unmoved.
        let (dx, dy) = if flags & 2 != 0 { (a1, a2) } else { (0.0, 0.0) };
        for contour in outline(glyf, ranges, glyph, depth + 1)? {
            out.push(
                contour
                    .into_iter()
                    .map(|(x, y, on)| (a * x + c * y + dx, b * x + d * y + dy, on))
                    .collect(),
            );
        }
        if flags & 0x20 == 0 {
            break;
        }
    }
    Ok(out)
}

/// The best Unicode subtable: full repertoire (format 12) first, then the BMP (format 4).
fn cmap(t: &Reader<'_>, glyph_count: usize) -> Result<BTreeMap<u32, u32>, String> {
    let count = t.u16(2)? as usize;
    let mut best: Option<(u32, usize)> = None;
    for i in 0..count {
        let platform = t.u16(4 + 8 * i)?;
        let encoding = t.u16(6 + 8 * i)?;
        let offset = t.u32(8 + 8 * i)? as usize;
        let format = t.u16(offset)?;
        let rank = match (platform, encoding, format) {
            (3, 10, 12) | (0, 4 | 6, 12) => 3,
            (3, 1, 4) | (0, 3, 4) => 2,
            (0, _, 4) => 1,
            _ => continue,
        };
        if best.is_none_or(|(r, _)| rank > r) {
            best = Some((rank, offset));
        }
    }
    let (_, offset) = best.ok_or("the font has no Unicode character map (cmap format 4 or 12)")?;
    let mut map = BTreeMap::new();
    let mut add = |c: u32, g: u32| {
        if g != 0 && (g as usize) < glyph_count && char::from_u32(c).is_some() {
            map.entry(c).or_insert(g);
        }
    };
    match t.u16(offset)? {
        12 => {
            let groups = t.u32(offset + 12)? as usize;
            for i in 0..groups {
                let at = offset + 16 + 12 * i;
                let (first, last, glyph) = (t.u32(at)?, t.u32(at + 4)?, t.u32(at + 8)?);
                if last < first || last > 0x10_FFFF {
                    return Err("a cmap group is malformed".into());
                }
                for c in first..=last {
                    add(c, glyph + (c - first));
                }
            }
        }
        _ => {
            let segments = t.u16(offset + 6)? as usize / 2;
            let ends = offset + 14;
            let starts = ends + 2 * segments + 2;
            let deltas = starts + 2 * segments;
            let range_offsets = deltas + 2 * segments;
            for s in 0..segments {
                let end = u32::from(t.u16(ends + 2 * s)?);
                let start = u32::from(t.u16(starts + 2 * s)?);
                let delta = u32::from(t.u16(deltas + 2 * s)?);
                let ro_at = range_offsets + 2 * s;
                let ro = t.u16(ro_at)? as usize;
                for c in start..=end.min(0xFFFE) {
                    let g = if ro == 0 {
                        (c + delta) & 0xFFFF
                    } else {
                        let g = u32::from(t.u16(ro_at + ro + 2 * (c - start) as usize)?);
                        if g == 0 { 0 } else { (g + delta) & 0xFFFF }
                    };
                    add(c, g);
                }
            }
        }
    }
    Ok(map)
}

/// The full name (name id 4), else the family (id 1).
fn name(t: &Reader<'_>) -> Option<String> {
    let count = t.u16(2).ok()? as usize;
    let strings = t.u16(4).ok()? as usize;
    let mut best: Option<(u32, String)> = None;
    for i in 0..count {
        let at = 6 + 12 * i;
        let platform = t.u16(at).ok()?;
        let id = t.u16(at + 6).ok()?;
        let len = t.u16(at + 8).ok()? as usize;
        let offset = t.u16(at + 10).ok()? as usize;
        let rank = match id {
            4 => 2,
            1 => 1,
            _ => continue,
        };
        let bytes = t.slice(strings + offset, len).ok()?;
        let text = match platform {
            0 | 3 => String::from_utf16_lossy(
                &bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&c| u16::from_be_bytes(c))
                    .collect::<Vec<_>>(),
            ),
            1 => bytes.iter().map(|&b| b as char).collect(),
            _ => continue,
        };
        if best.as_ref().is_none_or(|(r, _)| rank > *r) {
            best = Some((rank, text));
        }
    }
    best.map(|(_, s)| s)
}
