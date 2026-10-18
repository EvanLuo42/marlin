//! The skyline packer: no overlaps, inside the area, dense enough, and clear empties it.

use marlin_font::pack::{Rect, Skyline};
use marlin_math::Rng;

fn overlap(a: &Rect, b: &Rect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

#[test]
fn random_rectangles_never_overlap_and_fill_the_area() {
    let mut rng = Rng::new(0x5c1e);
    for round in 0..4 {
        let mut s = Skyline::new(256, 256);
        let mut placed: Vec<Rect> = Vec::new();
        let mut area = 0u64;
        // Glyph-like sizes: 4–24 wide, 6–30 tall.
        loop {
            let w = 4 + rng.below(21) as u32;
            let h = 6 + rng.below(25) as u32;
            let Some(r) = s.insert(w, h) else { break };
            assert_eq!((r.width, r.height), (w, h));
            assert!(
                r.x + r.width <= 256 && r.y + r.height <= 256,
                "round {round}: {r:?}"
            );
            for p in &placed {
                assert!(!overlap(p, &r), "round {round}: {p:?} and {r:?}");
            }
            area += u64::from(w * h);
            placed.push(r);
        }
        assert_eq!(s.used(), area);
        let density = area as f32 / (256.0 * 256.0);
        assert!(density > 0.7, "round {round}: {density}");
    }
}

#[test]
fn bottom_left_and_clear() {
    let mut s = Skyline::new(100, 100);
    assert_eq!(
        s.insert(60, 10),
        Some(Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10
        })
    );
    // The lowest top edge: beside the first, not on it.
    assert_eq!(
        s.insert(40, 20),
        Some(Rect {
            x: 60,
            y: 0,
            width: 40,
            height: 20
        })
    );
    assert_eq!(
        s.insert(50, 5),
        Some(Rect {
            x: 0,
            y: 10,
            width: 50,
            height: 5
        })
    );
    assert_eq!(s.insert(101, 1), None);
    assert_eq!(s.insert(0, 1), None);
    s.clear();
    assert_eq!(s.used(), 0);
    assert_eq!(
        s.insert(100, 100),
        Some(Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 100
        })
    );
    assert_eq!(s.insert(1, 1), None);
}
