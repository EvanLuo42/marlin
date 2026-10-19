//! Presampled light tiles (R3b): every frame's draws follow the lights' power, carry their
//! probability, leave out lights without power and the sun, and change with the seed.

use marlin_render::Environment;
use marlin_render::lights::{LIGHT_TILE_SIZE, LIGHT_TILES};
use marlin_render::scene::{LightDesc, LightId, LightKind, LightUpdate};

use crate::support::Harness;

fn light(kind: LightKind, candela: f32) -> LightDesc {
    LightDesc {
        kind,
        radiance: [candela; 3],
        radius: 0.0,
        position: [0.0; 3],
        direction: [0.0, -1.0, 0.0],
        range: 10.0,
        cone: [1.0, 0.5],
    }
}

fn add(h: &mut Harness, desc: LightDesc) -> LightId {
    let id = h.scene.ids().lights.reserve();
    h.updates.lights.push(id, LightUpdate::Add(desc));
    id
}

#[test]
fn draws_follow_the_lights_power() {
    let mut h = Harness::new(Environment::default());
    let candela = [1.0, 2.0, 3.0, 4.0, 10.0];
    let lit: Vec<LightId> = candela
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let kind = if i % 2 == 0 {
                LightKind::Point
            } else {
                LightKind::Spot
            };
            add(&mut h, light(kind, c))
        })
        .collect();
    let sun = add(&mut h, light(LightKind::Directional, 1000.0));
    let dark = add(&mut h, light(LightKind::Point, 0.0));
    let entries = h.light_tiles_frame(1).expect("lights to draw");
    let n = (LIGHT_TILES * LIGHT_TILE_SIZE) as usize;
    assert_eq!(entries.len(), n);
    let total: f32 = candela.iter().sum();
    for (id, c) in lit.iter().zip(candela) {
        let want = c / total;
        let drawn: Vec<_> = entries.iter().filter(|e| e.0 == id.index()).collect();
        assert!(drawn.iter().all(|e| (e.1 - want).abs() < 1e-6), "the pdf");
        // Within five standard deviations of the expected count.
        let expected = want as f64 * n as f64;
        let sigma = (expected * (1.0 - want as f64)).sqrt();
        assert!(
            (drawn.len() as f64 - expected).abs() < 5.0 * sigma,
            "light {}: {} drawn, {expected} expected",
            id.index(),
            drawn.len()
        );
    }
    assert!(
        entries
            .iter()
            .all(|e| e.0 != sun.index() && e.0 != dark.index()),
        "the sun and a dark light drawn"
    );
    // Each tile is a fair sample too: every tile draws the brightest light.
    let brightest = lit[4].index();
    for tile in entries.chunks(LIGHT_TILE_SIZE as usize) {
        assert!(tile.iter().filter(|e| e.0 == brightest).count() > 300);
    }
    // Another seed, other draws; the same seed, the same.
    let again = h.light_tiles_frame(1).unwrap();
    assert_eq!(again, entries);
    let other = h.light_tiles_frame(2).unwrap();
    assert_ne!(other, entries);
}

#[test]
fn removed_lights_leave_the_tiles() {
    let mut h = Harness::new(Environment::default());
    assert!(h.light_tiles_frame(0).is_none(), "no lights, no tiles");
    let a = add(&mut h, light(LightKind::Point, 1.0));
    let b = add(&mut h, light(LightKind::Point, 1.0));
    let entries = h.light_tiles_frame(0).unwrap();
    assert!(entries.iter().any(|e| e.0 == a.index()));
    h.updates.lights.push(a, LightUpdate::Remove);
    let entries = h.light_tiles_frame(0).unwrap();
    assert!(entries.iter().all(|e| e.0 == b.index() && e.1 == 1.0));
}
