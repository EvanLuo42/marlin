//! Command hashes (§5.5): the same drawing hashes the same however the buffers are laid out,
//! and every change that moves a pixel changes the hash of the command it belongs to.

use marlin_font::GlyphIds;
use marlin_math::Vec2;
use marlin_ui_render::command::Chunk;
use marlin_ui_render::damage::{command_hashes, frame_hash};
use marlin_ui_render::{
    Color, ExternalImageId, Gradient, GroupDesc, ImageColor, ImageSource, ImageStyle, Paint,
    Painter, Rect, RectStyle, Shadow, ShapeStyle, UiDrawList,
};

fn record(f: impl FnOnce(&mut Painter<'_>)) -> Chunk {
    let ids = GlyphIds::new();
    let mut list = UiDrawList::new(1.0);
    f(&mut list.painter(&ids));
    list.flatten()
}

fn base(p: &mut Painter<'_>) {
    p.rect(
        Rect::new(0.0, 0.0, 50.0, 50.0),
        &RectStyle::fill(Color::WHITE),
    );
    p.circle(Vec2::new(10.0, 10.0), 4.0, &ShapeStyle::fill(Color::BLACK));
}

#[test]
fn equal_drawings_hash_equal_and_changes_change_their_command() {
    let a = record(base);
    // The same drawing recorded through a slot: other buffer offsets, the same hashes.
    let ids = GlyphIds::new();
    let mut list = UiDrawList::new(1.0);
    let mut slot = list.painter(&ids).chunk();
    base(&mut slot.painter(&ids));
    list.fill(slot);
    let b = list.flatten();
    assert_eq!(command_hashes(&a, &[], &[]), command_hashes(&b, &[], &[]));

    let moved = record(|p| {
        p.rect(
            Rect::new(0.0, 0.0, 50.0, 50.0),
            &RectStyle::fill(Color::WHITE),
        );
        p.circle(Vec2::new(11.0, 10.0), 4.0, &ShapeStyle::fill(Color::BLACK));
    });
    let (ha, hm) = (
        command_hashes(&a, &[], &[]),
        command_hashes(&moved, &[], &[]),
    );
    assert_eq!(ha[0], hm[0]);
    assert_ne!(ha[1], hm[1]);
    assert!(ha.iter().all(|&h| h != 0));
    assert_ne!(frame_hash(&ha, 64, 64, 0), frame_hash(&hm, 64, 64, 0));
    assert_ne!(frame_hash(&ha, 64, 64, 0), frame_hash(&ha, 64, 65, 0));
}

#[test]
fn paints_groups_glyphs_externals_and_effect_members_count() {
    let gradient = |c: Color| {
        record(move |p| {
            let g = Gradient::linear(
                Vec2::ZERO,
                Vec2::new(9.0, 0.0),
                &[(0.0, Color::BLACK), (1.0, c)],
            );
            p.fill_rect(
                Rect::new(0.0, 0.0, 9.0, 9.0),
                marlin_ui_render::Corners::ZERO,
                &Paint::Gradient(&g),
            );
        })
    };
    assert_ne!(
        command_hashes(&gradient(Color::WHITE), &[], &[]),
        command_hashes(&gradient(Color::rgb(254, 255, 255)), &[], &[])
    );
    let grouped = |opacity: f32| {
        record(move |p| {
            p.begin_group(&GroupDesc {
                opacity,
                shadow: Some(Shadow {
                    offset: Vec2::ZERO,
                    blur: 2.0,
                    color: Color::BLACK,
                }),
                ..GroupDesc::default()
            });
            base(p);
            p.end_group();
        })
    };
    let (g1, g2) = (
        command_hashes(&grouped(1.0), &[], &[]),
        command_hashes(&grouped(0.5), &[], &[]),
    );
    assert!(
        g1.iter().zip(&g2).all(|(a, b)| a != b),
        "the group's opacity reaches every member"
    );
    // A member changing changes the shadow.
    let shadowed = |r: f32| {
        record(move |p| {
            p.begin_group(&GroupDesc {
                shadow: Some(Shadow {
                    offset: Vec2::ZERO,
                    blur: 2.0,
                    color: Color::BLACK,
                }),
                ..GroupDesc::default()
            });
            p.circle(Vec2::new(10.0, 10.0), r, &ShapeStyle::fill(Color::BLACK));
            p.end_group();
        })
    };
    assert_ne!(
        command_hashes(&shadowed(4.0), &[], &[])[0],
        command_hashes(&shadowed(5.0), &[], &[])[0]
    );
    // External images: their version.
    let image = record(|p| {
        p.image(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            ImageSource::External {
                id: ExternalImageId(3),
                width: 10,
                height: 10,
                color: ImageColor::LinearStraight,
            },
            &ImageStyle::default(),
        );
    });
    let v = |n: u64| command_hashes(&image, &[], &[(ExternalImageId(3), n)]);
    assert_eq!(v(1), v(1));
    assert_ne!(v(1), v(2));
    // Glyphs: where the glyph cache put them.
    let ids = GlyphIds::new();
    let font = crate::support::inter();
    let mut list = UiDrawList::new(1.0);
    let layout = marlin_font::TextLayout::new(&font, marlin_font::FontId(0), 12.0, "A");
    list.painter(&ids).text(&layout, Vec2::ZERO, Color::WHITE);
    let text = list.flatten();
    let id = text.payload[text.commands[0][3] as usize + 2] as usize;
    let mut table = vec![[0u32; 4]; id + 1];
    table[id] = [1, 2, 3, 0];
    let before = command_hashes(&text, &table, &[]);
    table[id] = [5, 2, 3, 0];
    assert_ne!(before, command_hashes(&text, &table, &[]));
}
