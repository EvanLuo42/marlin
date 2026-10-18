//! Fonts cooked from TrueType: a synthetic font checked point by point, the engine's Inter
//! checked for its metrics and outlines, and the cooker through the cook pipeline.

use marlin_asset::cooked::{Encoding, UnitView};
use marlin_cook_core::Cookers;
use marlin_cook_font::{FontCooker, cook_font};
use marlin_font::{Font, FontView, Segment};
use marlin_reflect::type_info;

use crate::support::{CookedUnit, Project, uuid};
use crate::synth::{Glyph, TestFont};

/// A 1000-unit font: glyph 1 a square, 2 a diamond of off-curve points only, 3 the square moved
/// right and at half size.
fn test_font() -> TestFont {
    TestFont {
        units_per_em: 1000,
        glyphs: vec![
            (500, Glyph::Empty),
            (
                600,
                Glyph::Simple(vec![vec![
                    (100, 0, true),
                    (500, 0, true),
                    (500, 400, true),
                    (100, 400, true),
                ]]),
            ),
            (
                700,
                Glyph::Simple(vec![vec![
                    (300, 0, false),
                    (600, 300, false),
                    (300, 600, false),
                    (0, 300, false),
                ]]),
            ),
            (650, Glyph::Composite(vec![(1, 200, 100, 0.5)])),
        ],
        cmap: vec![
            (u16::from(b'A'), 1),
            (u16::from(b'B'), 2),
            (u16::from(b'C'), 3),
        ],
    }
}

fn segments(font: &FontView<'_>, glyph: u32) -> Vec<Segment> {
    font.segments(glyph).collect()
}

fn line(p0: [f32; 2], p1: [f32; 2]) -> Segment {
    Segment { p0, c: p0, p1 }
}

#[test]
fn synthetic_font_cooks_point_by_point() {
    let payload = cook_font(&test_font().bytes(), 0).unwrap();
    let font = FontView::new(&payload).unwrap();
    assert_eq!(font.header.units_per_em, 1000);
    assert_eq!(font.header.glyph_count, 4);
    assert_eq!(
        (
            font.header.ascender,
            font.header.descender,
            font.header.line_gap
        ),
        (800, -200, 50)
    );
    assert_eq!(font.glyph_of('A'), Some(1));
    assert_eq!(font.glyph_of('C'), Some(3));
    assert_eq!(font.glyph_of('Z'), None);
    assert_eq!(font.glyph_or_missing('Z'), 0);
    assert_eq!(font.advance(2), 700.0);

    // Straight edges: control point at the start.
    assert_eq!(
        segments(&font, 1),
        vec![
            line([100.0, 0.0], [500.0, 0.0]),
            line([500.0, 0.0], [500.0, 400.0]),
            line([500.0, 400.0], [100.0, 400.0]),
            line([100.0, 400.0], [100.0, 0.0]),
        ]
    );
    let g = font.glyph(1);
    assert_eq!((g.x_min, g.y_min, g.x_max, g.y_max), (100, 0, 500, 400));

    // Off-curve points only: on-curve points implied halfway, the first between points 0 and 1.
    let mid = |a: [f32; 2], b: [f32; 2]| [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
    let p = [[300.0, 0.0], [600.0, 300.0], [300.0, 600.0], [0.0, 300.0]];
    let diamond = segments(&font, 2);
    assert_eq!(diamond.len(), 4);
    for (i, s) in diamond.iter().enumerate() {
        assert_eq!(s.c, p[(i + 1) % 4], "segment {i}");
        assert_eq!(s.p0, mid(p[i], p[(i + 1) % 4]));
        assert_eq!(s.p1, mid(p[(i + 1) % 4], p[(i + 2) % 4]));
    }

    // The composite: the square at half size, moved by (200, 100).
    let placed: Vec<Segment> = segments(&font, 1)
        .into_iter()
        .map(|s| {
            let f = |p: [f32; 2]| [p[0] * 0.5 + 200.0, p[1] * 0.5 + 100.0];
            Segment {
                p0: f(s.p0),
                c: f(s.c),
                p1: f(s.p1),
            }
        })
        .collect();
    assert_eq!(segments(&font, 3), placed);
    assert_eq!(segments(&font, 0), vec![]);
}

fn inter() -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../content/engine/fonts/Inter-Regular.ttf");
    std::fs::read(path)
        .expect("content/engine/fonts/Inter-Regular.ttf (Git LFS: run `git lfs pull`)")
}

#[test]
fn inter_cooks_with_its_metrics_and_outlines() {
    let payload = cook_font(&inter(), 0).unwrap();
    let font = FontView::new(&payload).unwrap();
    assert_eq!(font.header.units_per_em, 2048);
    assert!(font.name.contains("Inter"), "{}", font.name);
    assert!(font.header.glyph_count > 2000);
    assert!(font.header.ascender > 1500 && font.header.descender < -300);
    assert!(font.header.cap_height > 1400 && font.header.x_height > 1000);
    for c in ('a'..='z')
        .chain('A'..='Z')
        .chain('0'..='9')
        .chain(['é', 'ß', '€', '→'])
    {
        let g = font
            .glyph_of(c)
            .unwrap_or_else(|| panic!("{c:?} is mapped"));
        assert!(font.advance(g) > 0.0, "{c:?} advances");
        assert!(font.segments(g).next().is_some(), "{c:?} has an outline");
    }
    // A space has an advance and no outline.
    let space = font.glyph_of(' ').unwrap();
    assert!(font.advance(space) > 0.0 && font.segments(space).next().is_none());
    // 'O' is two contours; 'é' (a composite in many fonts) has the e's and the accent's.
    let o = font.glyph(font.glyph_of('O').unwrap());
    assert_eq!(o.contour_count, 2);
    assert!(font.glyph(font.glyph_of('é').unwrap()).contour_count >= 3);
    // Segments chain: each starts where the previous one ended, within a contour.
    let segs: Vec<_> = font.segments(font.glyph_of('S').unwrap()).collect();
    for w in segs.windows(2) {
        assert_eq!(w[0].p1, w[1].p0);
    }
    assert_eq!(segs.last().unwrap().p1, segs[0].p0);
    // The bounding box holds every point.
    let g = font.glyph(font.glyph_of('g').unwrap());
    for s in font.segments(font.glyph_of('g').unwrap()) {
        for p in [s.p0, s.c, s.p1] {
            assert!(p[0] >= f32::from(g.x_min) && p[0] <= f32::from(g.x_max));
            assert!(p[1] >= f32::from(g.y_min) && p[1] <= f32::from(g.y_max));
        }
    }
    assert!(g.y_min < 0, "g descends");
}

#[test]
fn cff_and_garbage_are_refused() {
    let mut otto = test_font().bytes();
    otto[0..4].copy_from_slice(b"OTTO");
    assert!(cook_font(&otto, 0).unwrap_err().contains("CFF"));
    assert!(cook_font(b"not a font at all", 0).is_err());
    assert!(cook_font(&test_font().bytes()[..40], 0).is_err());
    assert!(
        cook_font(&test_font().bytes(), 1)
            .unwrap_err()
            .contains("collections")
    );
}

fn payload(unit: &CookedUnit) -> &[u8] {
    let view = UnitView::new(&unit.bytes).unwrap();
    let object = view.object(0).unwrap();
    assert_eq!(object.type_id, type_info::<Font>().id.0);
    assert_eq!(object.encoding, Encoding::Cooked);
    view.payload(&object)
}

#[test]
fn fonts_cook_through_the_pipeline() {
    let p = Project::new("fonts");
    p.write("fonts/test.ttf", test_font().bytes());
    p.write(
        "fonts/test.masset",
        format!(
            "!Version 1\nFont {{ !Uuid = \"{}\" !Name = \"TestFont\" Source = `source:fonts/test.ttf` }}\n",
            uuid(0xf0)
        ),
    );
    let mut cookers = Cookers::new();
    cookers.register_named::<Font>(FontCooker).unwrap();
    let units = p.cook(&cookers).unwrap();
    assert_eq!(units.len(), 1);
    let font = FontView::new(payload(&units[0])).unwrap();
    assert_eq!(font.glyph_of('B'), Some(2));

    p.write(
        "fonts/test.masset",
        format!(
            "!Version 1\nFont {{ !Uuid = \"{}\" !Name = \"TestFont\" }}\n",
            uuid(0xf0)
        ),
    );
    let errors = p.cook(&cookers).unwrap_err().join("\n");
    assert!(errors.contains("`Source` is not set"), "{errors}");
}
