//! A minimal TrueType writer for test fonts: `head`, `maxp`, `hhea`, `hmtx`, `loca`, `glyf`
//! and a format 4 `cmap`, with simple and composite glyphs.

pub enum Glyph {
    Empty,
    /// Contours of (x, y, on-curve) points.
    Simple(Vec<Vec<(i16, i16, bool)>>),
    /// Components: glyph, x and y offsets, uniform scale (1.0 for none).
    Composite(Vec<(u16, i16, i16, f32)>),
}

pub struct TestFont {
    pub units_per_em: u16,
    pub glyphs: Vec<(u16, Glyph)>,
    /// Character, glyph.
    pub cmap: Vec<(u16, u16)>,
}

fn be16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn glyf_entry(glyph: &Glyph) -> Vec<u8> {
    let mut g = Vec::new();
    match glyph {
        Glyph::Empty => {}
        Glyph::Simple(contours) => {
            let all: Vec<_> = contours.iter().flatten().collect();
            let (x0, y0) = (
                all.iter().map(|p| p.0).min().unwrap(),
                all.iter().map(|p| p.1).min().unwrap(),
            );
            let (x1, y1) = (
                all.iter().map(|p| p.0).max().unwrap(),
                all.iter().map(|p| p.1).max().unwrap(),
            );
            be16(&mut g, contours.len() as u16);
            for v in [x0, y0, x1, y1] {
                be16(&mut g, v as u16);
            }
            let mut end = 0u16;
            for c in contours {
                end += c.len() as u16;
                be16(&mut g, end - 1);
            }
            be16(&mut g, 0); // no instructions
            // Long coordinates for every point: flags without the short or same bits.
            for p in &all {
                g.push(u8::from(p.2));
            }
            let (mut px, mut py) = (0i16, 0i16);
            for p in &all {
                be16(&mut g, (p.0 - px) as u16);
                px = p.0;
            }
            for p in &all {
                be16(&mut g, (p.1 - py) as u16);
                py = p.1;
            }
        }
        Glyph::Composite(parts) => {
            be16(&mut g, 0xFFFF); // -1 contours
            for _ in 0..4 {
                be16(&mut g, 0);
            }
            for (i, &(glyph, dx, dy, scale)) in parts.iter().enumerate() {
                let mut flags = 0x1 | 0x2; // word args, xy values
                if scale != 1.0 {
                    flags |= 0x8;
                }
                if i + 1 < parts.len() {
                    flags |= 0x20;
                }
                be16(&mut g, flags);
                be16(&mut g, glyph);
                be16(&mut g, dx as u16);
                be16(&mut g, dy as u16);
                if scale != 1.0 {
                    be16(&mut g, (scale * 16384.0) as i16 as u16);
                }
            }
        }
    }
    while g.len() % 4 != 0 {
        g.push(0);
    }
    g
}

impl TestFont {
    pub fn bytes(&self) -> Vec<u8> {
        let n = self.glyphs.len() as u16;
        let mut head = vec![0u8; 54];
        head[18..20].copy_from_slice(&self.units_per_em.to_be_bytes());
        head[50..52].copy_from_slice(&1u16.to_be_bytes()); // long loca
        let mut maxp = vec![0u8; 6];
        maxp[0..4].copy_from_slice(&0x0000_5000u32.to_be_bytes());
        maxp[4..6].copy_from_slice(&n.to_be_bytes());
        let mut hhea = vec![0u8; 36];
        hhea[4..6].copy_from_slice(&800i16.to_be_bytes());
        hhea[6..8].copy_from_slice(&(-200i16).to_be_bytes());
        hhea[8..10].copy_from_slice(&50i16.to_be_bytes());
        hhea[34..36].copy_from_slice(&n.to_be_bytes());
        let mut hmtx = Vec::new();
        let mut glyf = Vec::new();
        let mut loca = Vec::new();
        for (advance, glyph) in &self.glyphs {
            be16(&mut hmtx, *advance);
            be16(&mut hmtx, 0);
            loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
            glyf.extend(glyf_entry(glyph));
        }
        loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());

        // cmap format 4: one segment per mapping (delta form), then the 0xFFFF terminator.
        let mut map = self.cmap.clone();
        map.sort();
        let segs = map.len() as u16 + 1;
        let mut sub = Vec::new();
        be16(&mut sub, 4);
        be16(&mut sub, 0); // length, patched below
        be16(&mut sub, 0);
        be16(&mut sub, segs * 2);
        for _ in 0..3 {
            be16(&mut sub, 0); // search hints: unused by readers that scan
        }
        for &(c, _) in &map {
            be16(&mut sub, c);
        }
        be16(&mut sub, 0xFFFF);
        be16(&mut sub, 0);
        for &(c, _) in &map {
            be16(&mut sub, c);
        }
        be16(&mut sub, 0xFFFF);
        for &(c, g) in &map {
            be16(&mut sub, g.wrapping_sub(c));
        }
        be16(&mut sub, 1);
        for _ in 0..segs {
            be16(&mut sub, 0);
        }
        let len = sub.len() as u16;
        sub[2..4].copy_from_slice(&len.to_be_bytes());
        let mut cmap = Vec::new();
        be16(&mut cmap, 0);
        be16(&mut cmap, 1);
        be16(&mut cmap, 3);
        be16(&mut cmap, 1);
        cmap.extend_from_slice(&12u32.to_be_bytes());
        cmap.extend(sub);

        let tables: [(&[u8; 4], Vec<u8>); 7] = [
            (b"cmap", cmap),
            (b"glyf", glyf),
            (b"head", head),
            (b"hhea", hhea),
            (b"hmtx", hmtx),
            (b"loca", loca),
            (b"maxp", maxp),
        ];
        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        be16(&mut out, tables.len() as u16);
        for _ in 0..3 {
            be16(&mut out, 0);
        }
        let mut offset = 12 + 16 * tables.len();
        let mut body = Vec::new();
        for (tag, data) in &tables {
            out.extend_from_slice(*tag);
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut d = data.clone();
            while d.len() % 4 != 0 {
                d.push(0);
            }
            offset += d.len();
            body.extend(d);
        }
        out.extend(body);
        out
    }
}
