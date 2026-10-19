//! Group shadows and glow (§4.3): their profiles, the members' combined shape, the box that
//! stands in for many members, and bounds that hold all their ink.

use marlin_font::GlyphIds;
use marlin_math::Vec2;
use marlin_ui_render::command::{Chunk, Kind};
use marlin_ui_render::reference::{bin, tile_counts};
use marlin_ui_render::shape::{Scene, Textures, erf, shade};
use marlin_ui_render::{
    Affine2, Color, Glow, GroupDesc, Painter, Rect, RectStyle, Shadow, ShapeStyle, UiDrawList,
};

struct NoTextures;

impl Textures for NoTextures {
    fn glyph(&self, _: u32, _: u32, _: u32) -> f32 {
        0.0
    }
    fn sample(&self, _: u32, _: u32, _: [f32; 2], _: f32) -> [f32; 4] {
        [0.0; 4]
    }
}

fn record(f: impl FnOnce(&mut Painter<'_>)) -> Chunk {
    let ids = GlyphIds::new();
    let mut list = UiDrawList::new(1.0);
    f(&mut list.painter(&ids));
    list.flatten()
}

fn alpha(chunk: &Chunk, i: usize, x: f32, y: f32) -> f32 {
    shade(
        &chunk.commands[i],
        &Scene::new(chunk, &[]),
        &NoTextures,
        x,
        y,
    )[3]
}

const PANEL: Rect = Rect::new(40.0, 40.0, 120.0, 100.0);

fn shadowed(desc: GroupDesc) -> Chunk {
    record(|p| {
        p.begin_group(&desc);
        p.rect(PANEL, &RectStyle::fill(Color::WHITE));
        p.end_group();
    })
}

#[test]
fn a_shadow_is_the_members_shape_blurred_and_offset() {
    let chunk = shadowed(GroupDesc {
        shadow: Some(Shadow {
            offset: Vec2::new(3.0, 5.0),
            blur: 8.0,
            color: Color::BLACK,
        }),
        ..GroupDesc::default()
    });
    // The effect comes first, then the member.
    assert_eq!(Kind::from_word(chunk.commands[0][0]), Some(Kind::Effect));
    assert_eq!(Kind::from_word(chunk.commands[1][0]), Some(Kind::Rect));
    // Along the bottom edge, away from the corners: 0.5 erfc(d / (σ √2)), σ = 4, d measured
    // from the moved edge (y = 105).
    let sigma = 4.0f32;
    for dy in [-6.0f32, -2.0, 0.0, 1.5, 4.0, 9.0] {
        let y = 105.0 + dy;
        let expected = 0.5 - 0.5 * erf(dy / (sigma * core::f32::consts::SQRT_2));
        let got = alpha(&chunk, 0, 80.0, y);
        assert!((got - expected).abs() < 1e-4, "{dy}: {got} vs {expected}");
    }
    // A sharp shadow without blur is the shape's coverage.
    let sharp = shadowed(GroupDesc {
        shadow: Some(Shadow {
            offset: Vec2::new(10.0, 0.0),
            blur: 0.0,
            color: Color::BLACK,
        }),
        ..GroupDesc::default()
    });
    assert_eq!(alpha(&sharp, 0, 129.5, 70.0), 1.0);
    assert_eq!(alpha(&sharp, 0, 130.5, 70.0), 0.0);
    assert!(erf(0.0).abs() < 1e-6 && (erf(3.0) - 0.999_977_9).abs() < 1e-5);
}

#[test]
fn a_glow_fades_over_its_radius() {
    let chunk = shadowed(GroupDesc {
        glow: Some(Glow {
            radius: 10.0,
            color: Color::rgb(80, 160, 255),
        }),
        ..GroupDesc::default()
    });
    for (d, expected) in [
        (-3.0f32, 1.0f32),
        (0.0, 1.0),
        (2.5, 0.5625),
        (5.0, 0.25),
        (10.0, 0.0),
        (14.0, 0.0),
    ] {
        let got = alpha(&chunk, 0, 120.0 + d, 70.0);
        assert!((got - expected).abs() < 1e-5, "{d}: {got}");
    }
}

#[test]
fn the_combined_shape_is_the_nearest_member() {
    // Two circles: the shadow between them follows both, not their box.
    let chunk = record(|p| {
        p.begin_group(&GroupDesc {
            shadow: Some(Shadow {
                offset: Vec2::ZERO,
                blur: 0.0,
                color: Color::BLACK,
            }),
            ..GroupDesc::default()
        });
        p.circle(Vec2::new(30.0, 30.0), 10.0, &ShapeStyle::fill(Color::WHITE));
        // A member in a nested, scaled group counts too.
        p.begin_group(&GroupDesc {
            transform: Affine2::translate(70.0, 30.0) * Affine2::scale(2.0, 2.0),
            ..GroupDesc::default()
        });
        p.circle(Vec2::ZERO, 5.0, &ShapeStyle::fill(Color::WHITE));
        p.end_group();
        p.end_group();
    });
    assert_eq!(alpha(&chunk, 0, 30.5, 30.5), 1.0);
    assert_eq!(alpha(&chunk, 0, 70.5, 30.5), 1.0);
    // Between them, inside their box but outside both: no shadow.
    assert_eq!(alpha(&chunk, 0, 50.5, 30.5), 0.0);
}

#[test]
fn many_members_or_the_flag_take_the_box() {
    let many = record(|p| {
        p.begin_group(&GroupDesc {
            shadow: Some(Shadow {
                offset: Vec2::ZERO,
                blur: 0.0,
                color: Color::BLACK,
            }),
            ..GroupDesc::default()
        });
        for i in 0..17 {
            let x = 10.0 + i as f32 * 6.0;
            p.circle(Vec2::new(x, 20.0), 2.0, &ShapeStyle::fill(Color::WHITE));
        }
        p.end_group();
    });
    // Between two dots: inside their box, so shadowed.
    assert_eq!(alpha(&many, 0, 13.5, 20.5), 1.0);
    let flagged = shadowed(GroupDesc {
        shadow: Some(Shadow {
            offset: Vec2::ZERO,
            blur: 0.0,
            color: Color::BLACK,
        }),
        approximate: Some(6.0),
        ..GroupDesc::default()
    });
    // The box with 6 px corners: the corner pixel of the panel is outside it.
    assert_eq!(alpha(&flagged, 0, 40.5, 40.5), 0.0);
    assert_eq!(alpha(&flagged, 0, 46.5, 46.5), 1.0);
}

#[test]
fn effects_are_listed_wherever_they_have_ink() {
    let chunk = record(|p| {
        p.begin_group(&GroupDesc {
            transform: Affine2::translate(20.0, 10.0) * Affine2::rotate(0.3),
            opacity: 0.8,
            shadow: Some(Shadow {
                offset: Vec2::new(4.0, 6.0),
                blur: 12.0,
                color: Color::rgba(0, 0, 0, 160),
            }),
            glow: Some(Glow {
                radius: 9.0,
                color: Color::rgb(255, 200, 0),
            }),
            approximate: None,
        });
        p.rect(
            Rect::new(10.0, 10.0, 60.0, 40.0),
            &RectStyle::rounded(Color::WHITE, 6.0),
        );
        p.circle(Vec2::new(70.0, 30.0), 8.0, &ShapeStyle::fill(Color::WHITE));
        p.end_group();
    });
    let (w, h) = (128, 96);
    let tiles = bin(&chunk, &[], w, h);
    let (tx, _) = tile_counts(w, h);
    let scene = Scene::new(&chunk, &[]);
    for y in 0..h {
        for x in 0..w {
            for (i, c) in chunk.commands[..2].iter().enumerate() {
                let a = shade(c, &scene, &NoTextures, x as f32 + 0.5, y as f32 + 0.5)[3];
                // Gaussian tails beyond 3σ are below a level of 8-bit alpha.
                if a > 1.0 / 512.0 {
                    let tile = &tiles[((y / 16) * tx + x / 16) as usize];
                    assert!(
                        tile.commands.contains(&(i as u32)),
                        "effect {i} at ({x}, {y}): {a}"
                    );
                }
            }
        }
    }
}
