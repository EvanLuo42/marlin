//! The pairing patterns: reciprocal, never a pixel with itself, at the distance asked for.

use marlin_cook_render::{cook_pairing, generate_pairing, pairing_sigma};
use marlin_math::Rng;
use marlin_render::asset::PairingPattern;
use marlin_render::asset::PairingPatternView;
use marlin_render::asset::pairing::unpack_offset;

/// The partner of `(x, y)` in a `size` pattern: where its offset points, wrapped.
fn partner(offsets: &[u16], size: i32, x: i32, y: i32) -> (i32, i32, i32, i32) {
    let (dx, dy) = unpack_offset(offsets[(y * size + x) as usize]);
    ((x + dx).rem_euclid(size), (y + dy).rem_euclid(size), dx, dy)
}

#[test]
fn every_pixel_is_its_partners_partner() {
    let size = 64;
    let offsets = generate_pairing(size as u32, 6.0, &mut Rng::new(7));
    for y in 0..size {
        for x in 0..size {
            let (px, py, dx, dy) = partner(&offsets, size, x, y);
            assert!((dx, dy) != (0, 0), "({x}, {y}) is paired with itself");
            let (qx, qy, ex, ey) = partner(&offsets, size, px, py);
            assert_eq!((qx, qy), (x, y), "({x}, {y}) → ({px}, {py}) → ({qx}, {qy})");
            // Exactly opposite, so the pairs hold on screen too, where nothing wraps.
            assert_eq!((ex, ey), (-dx, -dy));
        }
    }
}

#[test]
fn the_mean_distance_is_the_disks() {
    let radius = 30.0;
    let size = 254;
    let offsets = generate_pairing(size, pairing_sigma(radius), &mut Rng::new(1));
    let n = offsets.len() as f64;
    let (mut distance, mut mean_x, mut mean_y) = (0.0, 0.0, 0.0);
    for &o in &offsets {
        let (dx, dy) = unpack_offset(o);
        distance += f64::from(dx * dx + dy * dy).sqrt() / n;
        mean_x += f64::from(dx) / n;
        mean_y += f64::from(dy) / n;
    }
    // A uniform disk of radius r: 2r/3.
    let disk = 2.0 * f64::from(radius) / 3.0;
    assert!(
        (distance - disk).abs() < 0.05 * disk,
        "mean distance {distance}, the disk's {disk}"
    );
    // No direction preferred (every pair counts both ways, so this is exact).
    assert!(mean_x.abs() < 1e-9 && mean_y.abs() < 1e-9);
    // Normal-ish: the share within σ per axis (a normal's 68%) and a thin tail.
    let sigma = f64::from(pairing_sigma(radius));
    let within = offsets
        .iter()
        .filter(|&&o| f64::from(unpack_offset(o).0.abs()) <= sigma)
        .count() as f64
        / n;
    assert!((0.63..0.73).contains(&within), "{within} within σ");
}

#[test]
fn the_cooked_patterns_read_back() {
    let pattern = PairingPattern {
        sizes: vec![32, 30],
        radius: 6.0,
        ..PairingPattern::default()
    };
    let payload = cook_pairing(&pattern).unwrap();
    let view = PairingPatternView::new(&payload).unwrap();
    assert_eq!(view.sizes, &[32, 30]);
    assert_eq!(view.pattern(1).len(), 900);
    // The same seed, the same patterns.
    assert_eq!(cook_pairing(&pattern).unwrap(), payload);
    // Odd sides and too many patterns are refused.
    for sizes in [vec![31], vec![], vec![16; 5], vec![256]] {
        let bad = PairingPattern {
            sizes,
            ..pattern.clone()
        };
        assert!(cook_pairing(&bad).is_err());
    }
}
